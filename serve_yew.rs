use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use anyhow::Result;
#[cfg(feature = "compression")]
use async_compression::tokio::write::GzipEncoder;
use axum::{body::Body, extract::Query};
use bytes::Bytes;
use frontend::{Routable, Route, ServerApp, ServerAppProps};
use futures::{future::BoxFuture, FutureExt};
use http::{header, request::Parts, HeaderValue, Request, Response, StatusCode, Uri};
use rust_embed::RustEmbed;
use sea_orm::DatabaseConnection;
use stylist::manager::StyleManager;
use tokio::io::AsyncWriteExt;
use tower_service::Service;

use crate::query::{select_cognis, select_cognises};

async fn render(
    index_html_s: &str,
    url: String,
    queries: HashMap<String, String>,
    db: &DatabaseConnection,
) -> String {
    let (writer, reader) = stylist::manager::render_static();

    let all_cognises = match Route::recognize(&url) {
        Some(Route::Explore) => Some(select_cognises(db).await.unwrap()),
        _ => None,
    };

    let cognis = match Route::recognize(&url) {
        Some(Route::Edit { id }) => Some(select_cognis(db, id).await.unwrap()),
        _ => None,
    };

    let body_s = yew::ServerRenderer::<ServerApp>::with_props(move || {
        let manager = StyleManager::builder()
            .writer(writer)
            .build()
            .expect("failed to create style manager.");
        let url = url.into();
        ServerAppProps {
            manager,
            url,
            queries,
            all_cognises,
            cognis,
        }
    })
    .render()
    .await;

    let data = reader.read_style_data();

    let mut style_s = String::new();
    data.write_static_markup(&mut style_s)
        .expect("failed to read styles from style manager");

    let head_split = index_html_s.split("</head>").collect::<Vec<_>>();
    let before_head = head_split[0];

    let after_head = head_split[1];

    let body_split = after_head.split("</body>").collect::<Vec<_>>();
    let before_body = body_split[0];
    let after_body = body_split[1];

    format!(
        "{}{}{}{}{}",
        before_head, style_s, before_body, body_s, after_body
    )
}

#[derive(RustEmbed)]
#[folder = "../frontend/dist/identity/"]
#[cfg_attr(not(feature = "compression"), include = "*")]
#[cfg_attr(feature = "compression", include = "assets/*")]
struct Asset;

use derive_more::{Deref, DerefMut, From};

#[derive(Debug, From, Deref, DerefMut)]
struct MimeMap(BTreeMap<Cow<'static, str>, HeaderValue>);

impl MimeMap {
    fn init() -> Self {
        let mut mimes = BTreeMap::new();

        let mut register_mime = |name: Cow<'static, str>| {
            mimes.insert(
                name.clone(),
                HeaderValue::from_static(
                    mime_guess::from_path(&*name)
                        .first_raw()
                        .unwrap_or_else(|| panic!("cannot guess mime type of {name}"))
                ),
            );
        };

        let files = Asset::iter();

        for file in files {
            register_mime(file);
        }

        #[cfg(feature = "compression")]
        {
            let files = BrotliTrunkPacked::iter();

            for file in files {
                let name = (file[..file.len() - 3]).to_owned();
                register_mime(Cow::Owned(name));
            }
        }

        mimes.into()
    }
}

#[cfg(feature = "compression")]
#[derive(Copy, Clone, RustEmbed)]
#[folder = "../frontend/dist/brotli/"]
#[exclude = "assets/*"]
struct BrotliTrunkPacked;

#[cfg(feature = "compression")]
#[derive(Copy, Clone, RustEmbed)]
#[folder = "../frontend/dist/brotli/assets/"]
struct BrotliAssets;

use rust_embed::EmbeddedFile;

impl Asset {
    /// get for real
    /// fixme: wrap RustEmbed struct and only expose this
    fn get_fr(path: &str) -> Option<(EmbeddedFile, Encoding)> {
        if path.starts_with("assets/") {
            // todo: automate this with a macro
            #[cfg(feature = "compression")]
            {
                if path.ends_with("logo.svg") {
                    return BrotliAssets::get("logo-6fe88bf3de22ed271405d7597167aa85.svg.br")
                        .map(|f| (f, Encoding::Brotli));
                }
            }

            return Self::get(path).map(|f| (f, Encoding::Identity));
        }

        #[cfg(feature = "compression")]
        {
            let path = format!("{}.br", path);

            BrotliTrunkPacked::get(&path).map(|f| (f, Encoding::Brotli))
        }
        #[cfg(not(feature = "compression"))]
        {
            Self::get(path).map(|f| (f, Encoding::Identity))
        }
    }

    fn get_index() -> Cow<'static, [u8]> {
        #[cfg(feature = "compression")]
        let data = include_bytes!("../../frontend/dist/identity/index.html")
            .as_slice()
            .into();
        #[cfg(not(feature = "compression"))]
        let data = Self::get("index.html").unwrap().data;
        data
    }
}

#[derive(Clone)]
pub struct ServeYew {
    db: DatabaseConnection,
    mime_map: Arc<MimeMap>,
}

impl ServeYew {
    /// Create a new [`ServeYew`].
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            db,
            mime_map: Arc::new(MimeMap::init()),
        }
    }

    fn get_asset(&self, path: &str) -> Option<(Bytes, HeaderValue, Encoding)> {
        let file = Asset::get_fr(path);

        file.map(|(f, e)| {
            let mime = self.mime_map.get(path).unwrap();

            let data = f.data.into_owned();

            (Bytes::from(data), mime.to_owned(), e)
        })
    }

    async fn get_file(&self, uri: &Uri) -> FileOutput {
        let (bytes, mime, encoding) = match self.get_asset(&uri.path()[1..]) {
            Some(asset) => asset,
            None => {
                let queries = Query::<HashMap<String, String>>::try_from_uri(uri).unwrap();
                let data = Asset::get_index();

                let rendered = render(
                    unsafe { std::str::from_utf8_unchecked(&data) },
                    uri.path().to_owned(),
                    queries.0,
                    &self.db,
                )
                .await;

                #[cfg(feature = "compression")]
                let rendered = {
                    let mut buf = Vec::new();
                    let mut encoder = GzipEncoder::new(&mut buf);
                    encoder.write_all(rendered.as_bytes()).await.unwrap();
                    encoder.shutdown().await.unwrap();
                    buf
                };

                (
                    Bytes::from(rendered),
                    HeaderValue::from_static("text/html"),
                    #[cfg(feature = "compression")]
                    Encoding::Gzip,
                    #[cfg(not(feature = "compression"))]
                    Encoding::Identity,
                )
            }
        };

        FileOutput {
            bytes,
            mime,
            encoding,
        }
    }
}

impl<ReqBody> Service<Request<ReqBody>> for ServeYew {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<'static, <ResponseFuture as Future>::Output>;
    // type Future = kin<Box<dyn Future<Output = <ResponseFuture as Future>::Output>>>;

    #[inline]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // build and validate the path
        let (Parts { uri, .. }, _body) = req.into_parts();

        if uri.path_and_query().is_none() {
            return Box::pin(ResponseFuture {
                inner: Inner::Invalid,
            });
        };

        let file = {
            use tracing::info;

            let assets = self.clone();
            async move {
                let path = uri.path();
                info!("Serving file from uri: {}", path);

                assets.get_file(&uri).await
            }
        };

        Box::pin(file.then(|f| ResponseFuture {
            inner: Inner::Valid(f),
        }))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Encoding {
    #[cfg(feature = "compression")]
    Gzip,
    #[cfg(feature = "compression")]
    Brotli,
    Identity,
}

impl Encoding {
    fn to_str(self) -> &'static str {
        match self {
            #[cfg(feature = "compression")]
            Encoding::Gzip => "gzip",
            #[cfg(feature = "compression")]
            Encoding::Brotli => "br",
            Encoding::Identity => "identity",
        }
    }

    fn into_header_value(self) -> HeaderValue {
        HeaderValue::from_static(self.to_str())
    }
}

struct FileOutput {
    bytes: Bytes,
    mime: HeaderValue,
    encoding: Encoding,
}

enum Inner {
    Valid(FileOutput),
    Invalid,
}

/// Response future of [`ServeYew`].
pub struct ResponseFuture {
    inner: Inner,
}

impl Future for ResponseFuture {
    type Output = Result<Response<Body>, Infallible>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &self.inner {
            Inner::Valid(file) => {
                let FileOutput {
                    bytes,
                    mime,
                    encoding,
                } = file;

                let mut res = Response::builder().body(Body::from(bytes.clone())).unwrap();
                let headers = res.headers_mut();
                headers.insert(header::CONTENT_TYPE, mime.clone());
                headers.insert(header::CONTENT_ENCODING, encoding.into_header_value());
                if mime == "text/html" {
                    headers.insert(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("max-age=0, private, must-revalidate"),
                    );
                } else {
                    headers.insert(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("public, max-age=31536000, immutable"),
                    );
                }

                Poll::Ready(Ok(res))
            }
            Inner::Invalid => {
                let res = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::empty())
                    .unwrap();

                Poll::Ready(Ok(res))
            }
        }
    }
}
