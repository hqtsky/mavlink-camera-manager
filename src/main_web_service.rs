mod helper;
mod mavlink_camera_information;

use actix_cors::Cors;
use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};

mod settings;

use std::sync::{Arc, Mutex};

use notify;

mod gst;

use dirs;

fn main() {
    let mut matches = clap::App::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .arg(
            clap::Arg::with_name("server")
                .short("s")
                .long("server")
                .value_name("IP:PORT")
                .help("Sets the IP and port that the rest server will be provided")
                .takes_value(true)
                .default_value("0.0.0.0:8088"),
        )
        .arg(
            clap::Arg::with_name("connect")
                .short("c")
                .long("connect")
                .value_name("TYPE:<IP/SERIAL>:<PORT/BAUDRATE>")
                .help("Sets the mavlink connection string, E.g: (udpbcast:192.168.0.255:14550)")
                .takes_value(true),
        )
        .get_matches();

    let server_string = matches.value_of("server").unwrap();
    println!("REST API address: {}", server_string);

    // Setting file
    let settings_path = format!(
        "{}/{}/",
        dirs::config_dir().unwrap().to_str().unwrap(),
        env!("CARGO_PKG_NAME")
    );
    let settings_file = "server-settings.toml";
    std::fs::create_dir_all(&settings_path)
        .unwrap_or_else(|error| panic!("Error while creating settings directory: {}", error));
    let settings = Arc::new(Mutex::new(settings::settings::Settings::new(
        std::path::Path::new(&settings_path)
            .join(&settings_file)
            .to_str()
            .unwrap(),
    )));

    if let Some(mavlink_string) = matches.value_of("connect") {
        settings.lock().unwrap().config.mavlink_endpoint = mavlink_string.to_string();
    }
    println!(
        "MAVLink connection in: {}",
        &settings.lock().unwrap().config.mavlink_endpoint
    );

    // Settings thread
    let settings_pipelines = settings.clone();
    let settings_rtsp = settings.clone();
    let settings_thread = settings.clone();

    // RTSP
    let mut rtsp = Arc::new(gst::rtsp_server::RTSPServer::default());
    let mut pipeline_runner = Arc::new(gst::pipeline_runner::PipelineRunner::default());

    // MAVLink communication thread
    let mut mavlink_camera = mavlink_camera_information::MavlinkCameraInformation::default();
    mavlink_camera.set_verbosity(true);
    mavlink_camera.connect(&settings.lock().unwrap().config.mavlink_endpoint);
    let mut mavlink_camera = Arc::new(mavlink_camera);

    let rtsp_settings = Arc::clone(&rtsp);
    let pipeline_runner_settings = Arc::clone(&pipeline_runner);
    let mavlink_camera_settings = Arc::clone(&mavlink_camera);
    std::thread::spawn(move || loop {
        // Avoid multiple file changes and allow a bigger time for the mutex to be unlocked
        std::thread::sleep(std::time::Duration::from_secs(1));

        let mut settings = settings_thread.lock().unwrap();
        match settings
            .file_channel
            .recv_timeout(std::time::Duration::from_millis(1))
        {
            Ok(notification) => match notification {
                notify::DebouncedEvent::Write(_) => {
                    settings.load();
                    println!("Settings file updated.");

                    // Stop RTSP or pipeline runner, this will force an restart
                    rtsp_settings.stop();
                    pipeline_runner_settings.stop();
                    let mut uri = settings
                        .config
                        .videos_configuration
                        .first()
                        .unwrap()
                        .endpoint
                        .clone()
                        .unwrap_or(Default::default());

                    uri = uri.replace("{service_ip}", &helper::get_ip_address_for_qgc());

                    println!("Video will be available in: {}", uri);
                    mavlink_camera_settings.set_video_stream_uri(uri)
                }
                _ => {}
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(x) => println!("Error in file settings update: {:#?}", x),
        }
    });

    {
        // This scope is necessary to use RAII of the mutex lock
        let config = &settings.lock().unwrap().config;
        mavlink_camera.set_video_stream_uri(
            config
                .videos_configuration
                .first()
                .unwrap()
                .endpoint
                .clone()
                .unwrap(),
        );
    }

    std::thread::spawn(move || loop {
        mavlink_camera.run_loop();
    });

    // Run RTSP or normal gstreamer pipeline, it depends of the content of the pipeline
    std::thread::spawn(move || loop {
        let pipeline = match settings_rtsp
            .lock()
            .unwrap()
            .config
            .videos_configuration
            .first()
        {
            Some(pipeline_struct) => pipeline_struct.pipeline.clone(),
            _ => None,
        };

        if pipeline.is_none() {
            println!("Pipeline string is empty.");
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }

        let pipeline = pipeline.unwrap();

        println!("Starting new pipeline: {:#?}", &pipeline);

        if pipeline.contains("name=pay") {
            let rtsp = Arc::make_mut(&mut rtsp);
            rtsp.set_pipeline(&pipeline);
            rtsp.run_loop();
        } else {
            let pipeline_runner = Arc::make_mut(&mut pipeline_runner);
            pipeline_runner.set_pipeline(&pipeline);
            pipeline_runner.run_loop();
        }
    });

    // REST API server
    HttpServer::new(move || {
        let settings_get_pipelines = Arc::clone(&settings_pipelines);
        let settings_post_pipelines = Arc::clone(&settings_pipelines);

        App::new()
            .wrap(Cors::default())
            .route(
                "/",
                web::get().to(move || {
                    "mavlink-camera-manager home page, WIP"
                }),
            )
            .route(
                "/pipelines",
                web::get().to(move |_: HttpRequest| {
                    let settings = settings_get_pipelines.lock().unwrap();

                    return HttpResponse::Ok()
                        .content_type("application/json")
                        .body(serde_json::to_string_pretty(&(*settings).config).unwrap());
                }),
            )
            .route(
                r"/pipelines/{id:\d+}",
                web::post().to(move |_: HttpRequest, id: web::Path<usize>, json: web::Json<settings::settings::VideoConfiguration>| {
                    let mut settings = settings_post_pipelines.lock().unwrap();
                    let number_of_configurations = settings.config.videos_configuration.len();
                    let id = id .into_inner();

                    if id > number_of_configurations {
                        return HttpResponse::NotFound()
                            .content_type("text/plain")
                            .body(format!("Maximum id allowed is {}", number_of_configurations));
                    }

                    if id == number_of_configurations {
                        settings.config.videos_configuration.push(json.into_inner());
                    } else {
                        settings.config.videos_configuration[id] = json.into_inner();
                    }

                    settings.save().unwrap_or_else(|error| {
                        println!("Failed to save file: {:#?}", error);
                    });

                    return HttpResponse::Ok()
                        .content_type("application/json")
                        .body(serde_json::to_string_pretty(&(*settings).config).unwrap());
                }),
            )
    })
    .bind(server_string)
    .unwrap()
    .run()
    .unwrap();
}
