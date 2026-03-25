use salvo::prelude::*;

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

#[tokio::main]
pub async fn main(){
    let router = Router::new().get(root);
    let listener=TcpListener::new("127.0.0.1:5800").bind().await;
    Server::new(listener).serve(router).await;
}
mod resource;
