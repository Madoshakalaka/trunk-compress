use gloo_net::eventsource::futures::EventSource;
use gloo_utils::format::JsValueSerdeExt;
use yew::use_effect;
use yew::use_mut_ref;

use futures::StreamExt;

#[yew::hook]
pub fn use_reload() {
    let version = use_mut_ref(|| None::<String>);

    use_effect(move || {
        let mut es = EventSource::new(&format!("/version",)).unwrap();
        let mut stream = es.subscribe("version").unwrap();
        yew::platform::spawn_local(async move {
            while let Some(Ok((_, msg))) = stream.next().await {
                let backend_version: String = msg.data().into_serde().unwrap();
                let current_version = version.borrow().clone();
                match current_version {
                    Some(v) => {
                        if v == backend_version {
                            gloo_console::info!(format!("backend version is the same: {}", v));
                        } else {
                            // refresh the page
                            web_sys::window().unwrap().location().reload().unwrap();
                        }
                    }
                    None => {
                        gloo_console::info!(format!("backend version: {}", backend_version));
                        *version.borrow_mut() = Some(backend_version);
                    }
                }
            }
        });

        // otherwise es gets dropped immediately and the connection won't even happen
        move || drop(es)
    });
}
