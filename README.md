This is what `trunk-compress` generates:

```
your_workspace/frontend/dist/
├── brotli
│   ├── assets
│   │   └── logo-6fe88bf3de22ed271405d7597167aa85.svg.br
│   ├── frontend-d8e8be0b5ce78d74_bg.wasm.br
│   ├── frontend-d8e8be0b5ce78d74.js.br
│   └── logo-686e460831c5276f.svg.br
└── identity
    ├── assets
    │   ├── logo.svg
    │   └── mebutlowquality.jpg
    ├── frontend-d8e8be0b5ce78d74_bg.wasm
    ├── frontend-d8e8be0b5ce78d74.js
    ├── index.html
    └── logo-686e460831c5276f.svg
```

And all these will embed into your server binary, with the files served from memory when you run your server.


# Features

- Trunk-compress avoids compressing videos and audios in the `assets` folder, it guesses the filetype through the suffix.
- Trunk-compress avoids compressing images in the `assets` folder, except for svgs.
- Trunk-compress generates compressed files with hashes attached to their filenames. When trunk-compress runs again, it will compare the hashes with those in the identity folder and remove only outdated compressed files, and avoid re-compressing already compressed files.
- Trunk-compress recognizes and uses hashes attached by trunk.
- The `serve-yew` service crate comes with a `/version` endpoint that returns the hash of the frontend.
- By using `/version` and SSE (server side events), we provide a frontend `use_reload` yew hook that will reload the page after a disconnect to the backend. This is ideal to reload your deployed apps when a new version is deployed to your production backend. It can also be used in development for hot-reloading.

# Usage

Try this template for a battery-included fast start <https://github.com/Madoshakalaka/trunk-compress-template>

Read on for more explanation.

> [!NOTE]  
> Not very configurable yet. PRs welcome.

To use this, you are expected to have the following directory structure,

```
your_workspace
├── backend
│   ├── build.rs
│   ├── Cargo.toml
│   └── src
│       └── ...
├── Cargo.toml
├── frontend
│   ├── assets
│   │   └── ...
│   ├── Cargo.toml
│   ├── dist
│   └── src
│       └── ...
└── ...
```

The name `assets` is important, it is expected to host media files like videos, images etc. it can be anywhere you want, as long as you have `<link data-trunk rel="copy-dir" href="path/to/your/assets"/>` in your `index.html`.

The name `frontend` is important.

It's recommended to add this to `Trunk.toml`:

```toml
[build]
dist = "frontend/dist/identity"
```

`build.rs` should look like the following:

```rs
fn main() {
    #[cfg(feature = "compression")]
    {
        use std::process::Command;
        // FYI: this seems always to be the parent of src, even if one runs cargo build at workspace root
        let current_dir = std::env::current_dir().unwrap();

        let workspace_root = current_dir.parent().unwrap();

        let compressor_exe = "trunk-compress";

        let frontend_dir = workspace_root.join("frontend/dist/identity");

        if compressor_exe.exists() {
            assert!(frontend_dir.exists(), "Please build frontend first");

            Command::new(&compressor_exe)
                .output()
                .expect("failed to compress files");
        }

        println!("cargo:rerun-if-changed={}", frontend_dir.to_str().unwrap());
    }
}
```

# Serve the files

Look at the `tower` service called `serve_yew::ServeYew` that serves this directory structure, which can be integrated to an axum service by:

```rs
let app = Router::new()
    // .nest("/api", api::routes())
    .fallback_service(yew::make_service(app_state.clone()));

mod yew{

    use std::{
        collections::{HashMap, HashSet},
    };

    use axum::{response::IntoResponse, RequestExt as _};
    use http::{HeaderName, HeaderValue};
    use serve_yew::{Process, ServeYew, WriteHeaders};
    // your own AppState
    use crate::AppState;

    // magic
    serve_yew::index!(INDEX);
    serve_yew::identity!(Files);
    serve_yew::brotli_code!(BrotliTrunkPacked);
    // if you don't have any compressable assets (like svg files), comment out the line below
    serve_yew::brotli_assets!(BrotliAssets);

    // these header values will be available to your render function
    fn interested_headers() -> HashSet<HeaderName> {
        let mut headers = HashSet::new();
        headers.insert(http::header::USER_AGENT);
        headers.insert(http::header::ACCEPT_LANGUAGE);
        headers
    }

    // if you don't have any assets, swap BrotliAssets with serve_yew::NoAssets
    #[cfg(feature = "compression")]
    pub fn make_service(s: AppState) -> ServeYew<Files, BrotliTrunkPacked, BrotliAssets, G, AppState> {
        use std::collections::BTreeMap;

        // todo: currently in compression mode, compressed assets have to be manually added
        let mut m = BTreeMap::new();
        m.insert("logo.svg", "logo-6fe88bf3de22ed271405d7597167aa85.svg.br");

        ServeYew::new(G, s, interested_headers(), m, INDEX)
    }

    #[cfg(not(feature = "compression"))]
    pub fn make_service(s: AppState) -> ServeYew<Files, G, AppState> {
        ServeYew::new(G, s, interested_headers())
    }

    // Your own middleware, which needs to implement `Process`, shown below
    #[derive(Clone)]
    pub struct G;

    // Your own cookie collection type, an example here.
    #[derive(Clone)]
    pub struct Cookies(SignedCookieJar, SignedCookieJar<TransientKey>);

    // How do you write cookie headers to the response? Example implementation here.
    impl WriteHeaders for Cookies {
        fn write_headers(&self, headers: &mut http::header::HeaderMap) {
            for (k, v) in self.0.clone().into_response().into_parts().0.headers {
                if let Some(k) = k {
                    headers.insert(k, v);
                }
            }
            for (k, v) in self.1.clone().into_response().into_parts().0.headers {
                if let Some(k) = k {
                    headers.insert(k, v);
                }
            }
        }
    }

    impl Process for G {
        type State = AppState;

        type Cookies = Cookies;

        // how do you extract cookies from the request?
        fn get_cookies(&self, request: axum::extract::Request, app_state: &Self::State) -> impl std::future::Future<Output = Self::Cookies> + Send {
            let mut request = request;
            async move {
                let signed_cookie_jar = request
                    .extract_parts_with_state::<SignedCookieJar<TransientKey>, _>(app_state)
                    .await
                    .unwrap();

                let cookie_jar = request.extract_with_state::<SignedCookieJar, _, _>(app_state).await.unwrap();
                Cookies(cookie_jar, signed_cookie_jar)
            }
        }

        // how do you render the response?
        fn render(
            &self,
            data: std::borrow::Cow<'static, [u8]>, // index.html
            path: String, // uri path
            queries: HashMap<String, String>,
            app_state: &Self::State,
            headers: HashMap<HeaderName, HeaderValue>, // your interested headers
            Cookies(cookie_jar, signed_cookie_jar): Self::Cookies,
        ) -> impl std::future::Future<Output = (String, Self::Cookies)> + Send {
            // SAFETY: it must be valid utf8 when included from the macro
            let html = unsafe { std::str::from_utf8_unchecked(&data) }.to_string();
            // ...

            async move {
                // ...
                let body_s = {

                    ::yew::ServerRenderer::<ServerApp>::with_props(move || {
                        ServerAppProps {
                          // ... Your SSR props
                          // possibly mutating cookies
                          // possibly utilizing headers
                          // possibly utilizing queries or path from the request uri
                        }
                    })
                }
                .render()
                .await;

                let final_html = todo!("normally you want some parsing to find the body tag in `html` and insert `body_s` there");

                (
                    final_html, // trunk-compress will serve your html with run-time compression.
                    Cookies(
                    // ...
                    ),
                )
            }
        }
    }
}
```

> [!IMPORTANT]  
> The macros expect your crate to have a `compression` feature. Use the following in your `Cargo.toml` Please.

```toml
[dependencies]
serve-yew = { git = "..." }

[features]
# name is important
compression = ["serve-yew/compression"]

# opt-in: shows a OS popup when a frontend is reloaded (useful in development)
reload = ["serve-yew/dev-reload"]
```

In your frontend, you should have this Cargo.toml:

```toml
[dependencies]
dev-reload = {git ="..."}
```


and this in your `App`:

```rs
#[function_component]
pub fn App() -> Html {
    dev_reload::use_reload();

    html! {
    // ...
    }
}
```


`ServeYew` serves the compressed file if it exists and sets the content type headers, cache headers etc

When `serve_yew/compression` is disabled, it serves everything uncompressed instead, useful in development.

# Something Not Expected?

you can manually run `trunk-compress` at the backend directory and see what exactly it has done. Here is an example of what it outputs:

```
❯ trunk-compress
2023-12-19T08:21:48.104332Z  WARN trunk_compress: removing outdated asset "../frontend/dist/brotli/assets/my-image-x9ysdfktryu3846f.svg.br" because of hash mismatch
2023-12-19T08:21:48.104385Z  INFO trunk_compress: removing outdated file "../frontend/dist/brotli/logo-68ye460831c5276f.svg.br" because can't find identity file
2023-12-19T08:21:48.104400Z  INFO trunk_compress: outputing target "../frontend/dist/brotli/assets/my-image-844dswidc8329904.svg.br"
2023-12-19T08:21:48.130450Z  INFO trunk_compress: Done compressing my-image.svg
2023-12-19T08:21:48.104520Z  INFO trunk_compress: outputing target "../frontend/dist/brotli/frontend-d8e8be0b5ce78d74.js.br"
2023-12-19T08:21:48.130719Z  INFO trunk_compress: Done compressing frontend-d8e8be0b5ce78d74.js
2023-12-19T08:21:48.130744Z  INFO trunk_compress: outputing target "../frontend/dist/brotli/frontend-d8e8be0b5ce78d74_bg.wasm.br"
2023-12-19T08:21:49.221114Z  INFO trunk_compress: Done compressing frontend-d8e8be0b5ce78d74_bg.wasm
2023-12-19T08:21:49.221145Z  INFO trunk_compress: outputing target "../frontend/dist/brotli/logo-686e460831c5276f.svg.br"
2023-12-19T08:21:49.229095Z  INFO trunk_compress: Done compressing logo-686e460831c5276f.svg
```


# Use it in Workflows

```yml
steps: 
  - name: Checkout Project
    uses: actions/checkout@v4

  - name: Setup trunk-compress
    run: wget -nv https://github.com/Madoshakalaka/trunk-compress/releases/latest/download/trunk-compress && chmod +x ./trunk-compress && mv trunk-compress /usr/local/bin

  - name: Setup Rust
    uses: dtolnay/rust-toolchain@master
    with:
      toolchain: nightly-2024-10-09
      targets: wasm32-unknown-unknown, x86_64-unknown-linux-musl
      components: clippy

  - name: Restore Rust Cache
    uses: Swatinem/rust-cache@v2

  - name: Setup trunk
    uses: jetli/trunk-action@v0.5.0
    with:
      version: 'latest'

  - name: build frontend
    run: mkdir -p frontend/dist/identity && mkdir -p frontend/dist/brotli trunk build --release 


  - name: Build Backend
    run: cargo build -p backend --features compression,journald --release 

  - uses: actions/upload-artifact@v4
    with:
      name: new-build
      path: target/release/backend

# Enoying deploying a single `backend` executible as your whole App!
# ...
```
