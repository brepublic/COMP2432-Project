use hotaru::prelude::*;
use hotaru::http::*;

mod models;
mod resource;

pub static APP: SApp = Lazy::new(|| {
    App::new()
        .binding("127.0.0.1:3000")
        .build()
});


endpoint! {
    APP.url("/"),
    pub index<HTTP> {
        let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>Sensor Dashboard</title>
</head>
<body>
    <h1>Sensor Data Server</h1>
    <p>Available endpoints:</p>
    <ul>
        <li><a href="/latest">GET /latest</a> – JSON of latest 10 frames</li>
        <li><a href="/stats">GET /stats</a> – system statistics </li>
        <li><code>GET /sensor/&lt;id&gt;</code> – filter by sensor </li>
    </ul>
    <p>Data is stored in <code>./data</code> directory.</p>
</body>
</html>"#;
        text_response(html)
    }
}
