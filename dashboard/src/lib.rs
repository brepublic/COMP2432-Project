use salvo::prelude::*;
mod models;

#[handler]
async fn root() -> Text<&'static str> {
    Text::Html(r#"<!DOCTYPE html>
<html>
<head>
    <title>Sensor Dashboard</title>
</head>
<body>
    <h1>Sensor Data Server</h1>
    <p>Available endpoints:</p>
    <ul>
        <li><a href="/latest">GET /latest</a> – JSON of latest 10 frames</li>
        <li><a href="/stats">GET /stats</a> – system statistics</li>
        <li><code>GET /sensor/&lt;id&gt;</code> – filter by sensor </li>
    </ul>
    <p>Data is stored in <code>./data</code> directory.</p>
</body>
</html>"#)
}


#[handler]
async fn latest() -> Text<&'static str> {
    Text::Html("<h1>latest</h1><p>to be implemented </p>")
}

#[handler]
async fn stats() -> Text<&'static str> {
    Text::Html("<h1>stats</h1><p>to be implemented </p>")
}

#[handler]
async fn thermo_1() -> Text<&'static str> {
    Text::Html("<h1>thermo_1</h1><p>to be implemented </p>")
}

#[handler]
async fn thermo_2() -> Text<&'static str> {
    Text::Html("<h1>thermo_2</h1><p>to be implemented </p>")
}

#[handler]
async fn accel_1() -> Text<&'static str> {
    Text::Html("<h1>accel_1</h1><p>to be implemented </p>")
}

#[handler]
async fn accel_2() -> Text<&'static str> {
    Text::Html("<h1>accel_2</h1><p>to be implemented </p>")
}


#[handler]
async fn force_1() -> Text<&'static str> {
    Text::Html("<h1>force_1</h1><p>to be implemented </p>")
}


pub async fn run(addr: &'static str){
    let sensor = Router::with_path("sensor")
                .push(Router::with_path("thermo_1").get(thermo_1))
                .push(Router::with_path("thermo_2").get(thermo_2))
                .push(Router::with_path("accel_1").get(accel_1))
                .push(Router::with_path("accel_2").get(accel_2))
                .push(Router::with_path("force_1").get(force_1));
    let router = Router::new()
                .get(root)
                .push(Router::with_path("stats").get(stats))
                .push(Router::with_path("latest").get(latest))
                .push(sensor);
    let listener=TcpListener::new(addr).bind().await;
    Server::new(listener).serve(router).await;
}