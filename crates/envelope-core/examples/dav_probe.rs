#[tokio::main]
async fn main() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let c = envelope_dav::DavClient::builder("http://127.0.0.1:5232/")
        .auth(envelope_dav::Auth::basic("alex@envelope.test", "password"))
        .build()
        .unwrap();
    let d = c
        .discover_from("http://127.0.0.1:5232/", envelope_dav::ServiceType::CalDav)
        .await;
    println!("discover: {d:?}");
    if let Ok(d) = d {
        let cals = c.list_calendars(&d.calendar_home_set[0]).await;
        println!("calendars: {cals:#?}");
    }
}
