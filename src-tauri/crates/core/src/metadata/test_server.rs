use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub(crate) struct Served {
    pub base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Served {
    /// How many connections the listener answered.
    pub(crate) fn hits(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The request line of the `index`th request, e.g.
    /// `GET /2.0/?method=… HTTP/1.1`.
    pub(crate) fn request_line(&self, index: usize) -> String {
        let requests = self.requests.lock().unwrap();
        requests[index]
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }
}

pub(crate) struct Reply<'a> {
    /// e.g. `"200 OK"`.
    pub status: &'a str,
    pub headers: &'a [&'a str],
    pub body: &'a str,
    /// Whether to send `content-length`. Without it the body runs to
    /// the end of the connection, which is the shape a ceiling that
    /// only trusted the header would let through.
    pub announce_length: bool,
}

pub(crate) async fn serve(reply: Reply<'_>) -> Served {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut response = format!("HTTP/1.1 {}\r\n", reply.status);
    for header in reply.headers {
        response.push_str(header);
        response.push_str("\r\n");
    }
    response.push_str("content-type: application/json\r\nconnection: close\r\n");
    if reply.announce_length {
        response.push_str(&format!("content-length: {}\r\n", reply.body.len()));
    }
    response.push_str("\r\n");
    response.push_str(reply.body);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut head = [0u8; 4096];
            let read = socket.read(&mut head).await.unwrap_or(0);
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&head[..read]).into_owned());
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    Served {
        base_url: format!("http://{address}"),
        requests,
    }
}

/// A `200 OK` carrying `body`, with its length announced.
pub(crate) async fn serve_json(body: &str) -> Served {
    serve(Reply {
        status: "200 OK",
        headers: &[],
        body,
        announce_length: true,
    })
    .await
}

/// The client a server would hand in: no redirect followed.
pub(crate) fn strict_http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}
