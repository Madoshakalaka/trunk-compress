This is what you get:

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

# Usage

warning: recently publicized internal code, not very configurable yet. PRs welcome.

To use this, you are expected to have the following directory structure,

```
your_workspace
├── ssr_server
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
│   │   └── identity
│   └── src
│       └── ...
└── ...
```

The name `assets` is important, it is expected to host media files like videos, images etc. it can be anywhere you want, as long as you have `<link data-trunk rel="copy-dir" href="path/to/your/assets"/>` in your `index.html`.

The name `frontend` is important.

dist must have an `identity` folder to start with. It's recommended to add this to `Trunk.toml`:

```toml
[build]
dist = "frontend/dist/identity"
```

`build.rs` should look like the following:

```rs
fn main() {
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


# Features

- It won't compress video and audio in the `assets` folder, it guesses the filetype through the suffix.
- It won't compress images in the `assets` folder, except svgs.
- It attaches hashes to the compressed asset files, when it runs again, it will compute the hashes of the identity files. Remove only outdated compressed files, and avoid re-compressing already compressed files.
- The smart behavior above also works for files packed by trunk, 
- It expects the identity files to have one suffix only, it may or may not work with files with multiple suffixes due to wonky parsing idk.

# Serve the files

I have a `tower` service called `ServeYew` that serves this directory structure, which can be integrated to an axum service by:

```rs
let app = Router::new()
    // .nest("/api", api::routes())
    .fallback_service(ServeYew::new(db.clone()));
```

The service serves the compressed file if it exists and sets the content type headers, cache headers etc

It also has a feature gate `compression` that when disabled, serves everything uncompressed instead, useful in local development environment.

It's not extracted into a crate yet.

Please refer to [serve_yew.rs]() and adapt to your own usage. If you use the compression feature, I recommend gating the `main()` in `build.rs` with `#[cfg(feature = "compression")]`
