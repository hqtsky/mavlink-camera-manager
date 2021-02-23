use super::pages;

use actix_web::{rt::System, web, App, HttpServer};

// Start REST API server with the desired address
pub fn run(server_address: &str) {
    let server_address = server_address.to_string();

    // Start HTTP server thread
    let _ = System::new("http-server");
    HttpServer::new(|| {
        App::new()
            .route("/html/{filename:.*}", web::get().to(pages::root))
            .route("/xml", web::post().to(pages::xml))
            .route("/v4l", web::get().to(pages::v4l))
            .route("/v4l", web::post().to(pages::v4l_post))
    })
    .bind(server_address)
    .unwrap()
    .run();
}
