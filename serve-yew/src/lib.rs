use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    convert::Infallible,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use ::rust_embed::RustEmbed;
use axum::{
    body::Body,
    extract::{Query, Request},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Sse,
    },
};
use futures::{FutureExt, Stream, StreamExt as _};

use bytes::Bytes;
use derive_more::{Deref, DerefMut, From};
use futures::future::BoxFuture;
use http::{header, HeaderName, HeaderValue, Response, StatusCode};
use rust_embed::{EmbeddedFile, Filenames};
use tower_service::Service;

#[macro_export]
macro_rules! identity {
    ($name:ident) => {
        #[derive(::rust_embed::RustEmbed, Clone)]
        #[folder = "../frontend/dist/identity/"]
        #[cfg_attr(not(feature = "compression"), include = "*")]
        #[cfg_attr(feature = "compression", include = "assets/*")]
        pub struct $name;
    };
}

#[macro_export]
macro_rules! brotli_code {
    ($name:ident) => {
        #[cfg(feature = "compression")]
        #[derive(Copy, Clone, ::rust_embed::RustEmbed)]
        #[folder = "../frontend/dist/brotli/"]
        #[exclude = "assets/*"]
        pub struct $name;
    };
}

#[macro_export]
macro_rules! brotli_assets {
    ($name:ident) => {
        #[cfg(feature = "compression")]
        #[derive(Copy, Clone, ::rust_embed::RustEmbed)]
        #[folder = "../frontend/dist/brotli/assets/"]
        pub struct $name;
    };
}

#[macro_export]
macro_rules! index {
    ($name:ident) => {
        #[cfg(feature = "compression")]
        const $name: &[u8] = include_bytes!("../../frontend/dist/identity/index.html");
    };
}

#[derive(Clone)]
pub struct NoAssets;

impl RustEmbed for NoAssets {
    fn get(_file_path: &str) -> Option<EmbeddedFile> {
        None
    }

    fn iter() -> Filenames {
        #[cfg(debug_assertions)]
        {
            Filenames::Dynamic(Box::new(std::iter::empty()))
        }
        #[cfg(not(debug_assertions))]
        {
            Filenames::Embedded([].iter())
        }
    }
}

#[derive(Debug, From, Deref, DerefMut)]
struct MimeMap(BTreeMap<Cow<'static, str>, HeaderValue>);

impl MimeMap {
    fn init<
        A: Iterator<Item = Cow<'static, str>>,
        #[cfg(feature = "compression")] C: Iterator<Item = Cow<'static, str>>,
    >(
        a: A,
        #[cfg(feature = "compression")] c: C,
    ) -> Self {
        let mut mimes = BTreeMap::new();

        let mut register_mime = |name: Cow<'static, str>| {
            mimes.insert(
                name.clone(),
                HeaderValue::from_static(
                    mime_guess::from_path(&*name)
                        .first_raw()
                        .unwrap_or_else(|| panic!("cannot guess mime type of {name}")),
                ),
            );
        };

        for file in a {
            register_mime(file);
        }

        #[cfg(feature = "compression")]
        {
            for file in c {
                let name = (file[..file.len() - 3]).to_owned();
                register_mime(Cow::Owned(name));
            }
        }

        mimes.into()
    }
}

pub trait Process {
    type State;
    type Cookies: Clone + WriteHeaders + Send + 'static;
    fn get_cookies(
        &self,
        request: Request,
        app_state: &Self::State,
    ) -> impl Future<Output = Self::Cookies> + Send;
    fn render(
        &self,
        data: Cow<'static, [u8]>,
        path: String,
        queries: HashMap<String, String>,
        app_state: &Self::State,
        extracted_headers: HashMap<HeaderName, HeaderValue>,
        cookies: Self::Cookies,
    ) -> impl Future<Output = (String, Self::Cookies)> + Send;
}

#[derive(Clone)]
pub struct ServeYew<
    A: RustEmbed + Clone + Send,
    #[cfg(feature = "compression")] C: RustEmbed + Clone + Send,
    #[cfg(feature = "compression")] C1: RustEmbed + Clone + Send,
    G: Process<State = S> + Clone + Send,
    S: Clone + Send,
> {
    _phantom: PhantomData<A>,
    #[cfg(feature = "compression")]
    _phantom2: PhantomData<C>,
    #[cfg(feature = "compression")]
    _phantom3: PhantomData<C1>,
    mime_map: Arc<MimeMap>,
    #[cfg(feature = "compression")]
    brotli_asset_mapping: BTreeMap<&'static str, &'static str>,
    #[cfg(feature = "compression")]
    index: &'static [u8],
    g: G,
    app_state: S,
    headers: HashSet<HeaderName>,
}

#[cfg(not(feature = "compression"))]
impl<A: RustEmbed + Clone + Send, G: Process<State = S> + Clone + Send, S: Clone + Send>
    ServeYew<A, G, S>
{
    pub fn get_version() -> String {
        // frontend-3c585650ceac9d6d.js for example
        A::iter()
            .find(|f| f.ends_with(".js"))
            .map(|f| {
                f.split('-')
                    .last()
                    .unwrap()
                    .trim_end_matches(".js")
                    .to_owned()
            })
            .unwrap()
    }

    pub fn new(g: G, app_state: S, headers: HashSet<HeaderName>) -> Self {
        Self {
            _phantom: PhantomData,
            mime_map: Arc::new(MimeMap::init(A::iter())),
            g,
            app_state,
            headers,
        }
    }
}

#[cfg(not(feature = "compression"))]
impl<A: RustEmbed + Clone + Send, G: Process<State = S> + Clone + Send, S: Clone + Send>
    ServeYew<A, G, S>
{
    fn get_asset(&self, path: &str) -> Option<(Bytes, HeaderValue, Encoding)> {
        let file = Self::get_fr(path);

        file.map(|(f, e)| {
            let mime = self.mime_map.get(path).unwrap();

            let data = f.data.into_owned();

            (Bytes::from(data), mime.to_owned(), e)
        })
    }

    fn get_fr(path: &str) -> Option<(EmbeddedFile, Encoding)> {
        if path.starts_with("assets/") {
            return A::get(path).map(|f| (f, Encoding::Identity));
        }

        A::get(path).map(|f| (f, Encoding::Identity))
    }
}

#[cfg(feature = "compression")]
// impl<A: RustEmbed, C: RustEmbed, C1: RustEmbed, G: Process<S>, S> ServeYew<A, C, C1, G, S> {
impl<
        A: RustEmbed + Clone + Send,
        C: RustEmbed + Clone + Send,
        C1: RustEmbed + Clone + Send,
        G: Process<State = S> + Clone + Send,
        S: Clone + Send,
    > ServeYew<A, C, C1, G, S>
{
    pub fn get_version() -> String {
        // frontend-3c585650ceac9d6d.js.br for example
        C::iter()
            .find(|f| f.ends_with(".js.br"))
            .map(|f| {
                f.split('-')
                    .last()
                    .unwrap()
                    .trim_end_matches(".js.br")
                    .to_string()
            })
            .unwrap()
    }

    fn get_asset(&self, path: &str) -> Option<(Bytes, HeaderValue, Encoding)> {
        let file = self.get_fr(path);

        file.map(|(f, e)| {
            let mime = self.mime_map.get(path).unwrap();

            let data = f.data.into_owned();

            (Bytes::from(data), mime.to_owned(), e)
        })
    }

    fn get_fr(&self, path: &str) -> Option<(EmbeddedFile, Encoding)> {
        if path.starts_with("assets/") {
            // todo: automate this with a macro
            if let Some(&file) = self.brotli_asset_mapping.get(path) {
                return C1::get(file).map(|f| (f, Encoding::Brotli));
            }

            return A::get(path).map(|f| (f, Encoding::Identity));
        }

        let path = format!("{}.br", path);

        C::get(&path).map(|f| (f, Encoding::Brotli))
    }
}

#[cfg(feature = "compression")]
impl<
        A: RustEmbed + Clone + Send,
        C: RustEmbed + Clone + Send,
        C1: RustEmbed + Clone + Send,
        G: Process<State = S> + Clone + Send,
        S: Clone + Send,
    > ServeYew<A, C, C1, G, S>
{
    pub fn new(
        g: G,
        app_state: S,
        headers: HashSet<HeaderName>,
        brotli_asset_mapping: BTreeMap<&'static str, &'static str>,
        index: &'static [u8],
    ) -> Self {
        Self {
            _phantom: PhantomData,
            _phantom2: PhantomData,
            _phantom3: PhantomData,
            mime_map: Arc::new(MimeMap::init(A::iter(), C::iter())),
            brotli_asset_mapping,
            index,
            g,
            app_state,
            headers,
        }
    }
}

#[cfg(feature = "compression")]
impl<
        A: RustEmbed + Clone + Send + 'static,
        C: RustEmbed + Clone + Send + 'static,
        C1: RustEmbed + Clone + Send + 'static,
        P: Process<State = S> + Clone + Send + 'static,
        S: Clone + Send + 'static,
    > Service<Request> for ServeYew<A, C, C1, P, S>
{
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<'static, <ResponseFuture<P::Cookies> as Future>::Output>;

    #[inline]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let uri = req.uri().clone();

        if uri.path_and_query().is_none() {
            return Box::pin(ResponseFuture {
                inner: Inner::Invalid::<P::Cookies>,
            });
        };

        let s = self.clone();
        async move {
            if let Some(o) = return_on_version(&req, Self::get_version) {
                return o;
            }

            let extracted_headers = extracted_headers(&req, &s.headers);
            let out = s.g.get_cookies(req, &s.app_state).await;

            let (bytes, mime, encoding, cookie_jars) = match s.get_asset(&uri.path()[1..]) {
                Some(asset) => (asset.0, asset.1, asset.2, out),
                None => {
                    let queries = Query::<HashMap<String, String>>::try_from_uri(&uri).unwrap();

                    let (rendered, cookie_jars) =
                        s.g.render(
                            s.index.into(),
                            uri.path().to_owned(),
                            queries.0,
                            &s.app_state,
                            extracted_headers,
                            out,
                        )
                        .await;

                    use {async_compression::tokio::write::GzipEncoder, tokio::io::AsyncWriteExt};

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
                        Encoding::Gzip,
                        cookie_jars,
                    )
                }
            };

            TheOutput::Other {
                bytes,
                mime,
                encoding,
                cookie_jars,
            }
        }
        .then(|o| ResponseFuture {
            inner: Inner::Valid(o),
        })
        .boxed()
    }
}

fn return_on_version<C: WriteHeaders, G: FnOnce() -> String>(
    req: &Request,
    g: G,
) -> Option<TheOutput<C>> {
    if req.uri().path().starts_with("/version") {
        Some(TheOutput::Version(g()))
    } else {
        None
    }
}

fn extracted_headers(
    req: &Request,
    headers: &HashSet<HeaderName>,
) -> HashMap<HeaderName, HeaderValue> {
    req.headers()
        .iter()
        .filter(|(k, _)| headers.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(not(feature = "compression"))]
impl<
        A: RustEmbed + Clone + Send + 'static,
        P: Process<State = S> + Clone + Send + 'static,
        S: Clone + Send + 'static,
    > Service<Request> for ServeYew<A, P, S>
{
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<'static, <ResponseFuture<P::Cookies> as Future>::Output>;

    #[inline]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        // let uri = req.uri().clone();

        if req.uri().path_and_query().is_none() {
            return Box::pin(ResponseFuture {
                inner: Inner::Invalid::<P::Cookies>,
            });
        };

        let s = self.clone();
        async move {
            if let Some(o) = return_on_version(&req, Self::get_version) {
                return o;
            }
            let uri = req.uri().clone();

            let extracted_headers = extracted_headers(&req, &s.headers);

            let out = s.g.get_cookies(req, &s.app_state).await;

            let (bytes, mime, encoding, cookie_jars) = match s.get_asset(&uri.path()[1..]) {
                Some(asset) => (asset.0, asset.1, asset.2, out),
                None => {
                    let queries = Query::<HashMap<String, String>>::try_from_uri(&uri).unwrap();

                    let data: Cow<'static, [u8]> = A::get("index.html").unwrap().data;

                    let (rendered, cookie_jars) =
                        s.g.render(
                            data,
                            uri.path().to_owned(),
                            queries.0,
                            &s.app_state,
                            extracted_headers,
                            out,
                        )
                        .await;

                    (
                        Bytes::from(rendered),
                        HeaderValue::from_static("text/html"),
                        Encoding::Identity,
                        cookie_jars,
                    )
                }
            };

            TheOutput::Other {
                bytes,
                mime,
                encoding,
                cookie_jars,
            }
        }
        .then(|f| ResponseFuture {
            inner: Inner::Valid(f),
        })
        .boxed()
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

enum TheOutput<C> {
    Version(String),
    Other {
        bytes: Bytes,
        mime: HeaderValue,
        encoding: Encoding,
        cookie_jars: C,
    },
}

enum Inner<C> {
    Valid(TheOutput<C>),
    Invalid,
}

pub struct ResponseFuture<C> {
    inner: Inner<C>,
}

pub trait WriteHeaders {
    fn write_headers(&self, headers: &mut http::header::HeaderMap);
}

const NO_CACHE: HeaderValue = HeaderValue::from_static("max-age=0, private, must-revalidate");

impl<C: Clone + WriteHeaders> Future for ResponseFuture<C> {
    type Output = Result<Response<Body>, Infallible>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &self.inner {
            Inner::Valid(TheOutput::Version(v)) => {
                let mut res = version(v.clone()).into_response();
                res.headers_mut().insert(header::CACHE_CONTROL, NO_CACHE);
                Poll::Ready(Ok(res))
            }
            Inner::Valid(TheOutput::Other {
                bytes,
                mime,
                encoding,
                cookie_jars,
            }) => {
                let mut res = Body::from(bytes.clone()).into_response();
                cookie_jars.write_headers(res.headers_mut());

                let headers = res.headers_mut();
                headers.insert(header::CONTENT_TYPE, mime.clone());
                headers.insert(header::CONTENT_ENCODING, encoding.into_header_value());
                if mime == "text/html" {
                    headers.insert(header::CACHE_CONTROL, NO_CACHE);
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

fn version(v: String) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    #[cfg(feature = "dev-reload")]
    notify_rust::Notification::new().summary("connected");

    let version_event =
        futures::stream::once(async move { Ok(Event::default().event("version").data(v)) });

    let stream = version_event.chain(futures::stream::pending());

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
