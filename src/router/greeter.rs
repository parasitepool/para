use super::*;

const GREET_TIMEOUT: Duration = Duration::from_secs(3);
const GREET_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);
const GREET_MAX_MESSAGES: usize = 8;

#[derive(Default)]
pub(crate) struct Prelude {
    pub(crate) inbox: VecDeque<Message>,
    pub(crate) resume_enonce1: Option<Extranonce>,
    pub(crate) suggested_difficulty: Option<Difficulty>,
}

pub(crate) async fn greet(
    mut reader: FramedRead<OwnedReadHalf, LinesCodec>,
    addr: SocketAddr,
) -> Option<(FramedRead<OwnedReadHalf, LinesCodec>, Prelude)> {
    let mut prelude = Prelude::default();

    loop {
        if prelude.inbox.len() >= GREET_MAX_MESSAGES {
            break;
        }

        let wait = if prelude.inbox.is_empty() {
            GREET_TIMEOUT
        } else {
            GREET_DRAIN_TIMEOUT
        };

        let line = match timeout(wait, reader.next()).await {
            Ok(Some(Ok(line))) => line,
            Ok(Some(Err(err))) => {
                warn!("Greet read error from {addr}: {err}");
                return None;
            }
            Ok(None) => {
                debug!("Client {addr} disconnected during greet");
                return None;
            }
            Err(_) => {
                if prelude.inbox.is_empty() {
                    debug!("Greet timed out waiting for first message from {addr}");
                    return None;
                }
                break;
            }
        };

        match serde_json::from_str::<Message>(&line) {
            Ok(message) => {
                match &message {
                    Message::Request { method, .. } | Message::Notification { method } => {
                        match method {
                            Method::Subscribe(subscribe) => {
                                prelude.resume_enonce1 = subscribe.enonce1.clone();
                            }
                            Method::SuggestDifficulty(suggest) => {
                                prelude.suggested_difficulty = Some(suggest.difficulty());
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }

                prelude.inbox.push_back(message);
            }
            Err(err) => {
                warn!("Invalid stratum message during greet from {addr}: {err}");
                return None;
            }
        }
    }

    debug!(
        "Greeted {addr}: resume={:?} suggested={:?} buffered={}",
        prelude.resume_enonce1,
        prelude.suggested_difficulty,
        prelude.inbox.len(),
    );

    Some((reader, prelude))
}

#[cfg(test)]
mod tests {
    use {super::*, tokio::net::TcpStream};

    type Greeted = (FramedRead<OwnedReadHalf, LinesCodec>, Prelude);

    fn framed(stream: TcpStream) -> FramedRead<OwnedReadHalf, LinesCodec> {
        let (read_half, _) = stream.into_split();
        FramedRead::new(read_half, LinesCodec::new_with_max_length(MAX_MESSAGE_SIZE))
    }

    async fn greeted(line: &str) -> Option<Greeted> {
        greeted_lines(&[line]).await
    }

    async fn greeted_lines(lines: &[&str]) -> Option<Greeted> {
        use tokio::io::AsyncWriteExt;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let payload = lines.concat();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream.write_all(payload.as_bytes()).await.unwrap();
            stream
        });

        let (stream, peer) = listener.accept().await.unwrap();
        let greeted = greet(framed(stream), peer).await;

        drop(client);

        greeted
    }

    #[tokio::test(start_paused = true)]
    async fn greet_captures_prelude_signals() {
        let (_, prelude) = greeted_lines(&[
            "{\"id\":1,\"method\":\"mining.suggest_difficulty\",\"params\":[1000]}\n",
            "{\"id\":2,\"method\":\"mining.subscribe\",\"params\":[\"foo\",\"deadbeef\"]}\n",
        ])
        .await
        .unwrap();

        assert_eq!(prelude.suggested_difficulty, Some(Difficulty::from(1000)));
        assert_eq!(prelude.resume_enonce1, Some("deadbeef".parse().unwrap()));
        assert_eq!(prelude.inbox.len(), 2);
        assert!(matches!(
            &prelude.inbox[0],
            Message::Request {
                method: Method::SuggestDifficulty(_),
                ..
            }
        ));
        assert!(matches!(
            &prelude.inbox[1],
            Message::Request {
                method: Method::Subscribe(_),
                ..
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn greet_buffers_non_signal_messages() {
        let (_, prelude) = greeted("{\"id\":1,\"method\":\"mining.foo\",\"params\":[]}\n")
            .await
            .unwrap();

        assert_eq!(prelude.inbox.len(), 1);
        assert!(prelude.resume_enonce1.is_none());
        assert!(prelude.suggested_difficulty.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_silent_probes_greet_independently() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut clients = Vec::new();

        for _ in 0..2 {
            clients.push(tokio::spawn(async move {
                let _stream = TcpStream::connect(addr).await.unwrap();
                sleep(Duration::from_secs(3600)).await;
            }));
        }

        let mut greets = Vec::new();

        for _ in 0..2 {
            let (stream, peer) = listener.accept().await.unwrap();
            greets.push(tokio::spawn(
                async move { greet(framed(stream), peer).await },
            ));
        }

        let start = tokio::time::Instant::now();

        for handle in greets {
            assert!(handle.await.unwrap().is_none());
        }

        assert_eq!(start.elapsed(), GREET_TIMEOUT);

        for client in clients {
            client.abort();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn greet_drops_invalid_message() {
        assert!(greeted("garbage\n").await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn greet_caps_buffered_messages() {
        let lines = vec![
            "{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"foo\"]}\n";
            GREET_MAX_MESSAGES + 2
        ];

        let (_, prelude) = greeted_lines(&lines).await.unwrap();

        assert_eq!(prelude.inbox.len(), GREET_MAX_MESSAGES);
    }

    #[tokio::test(start_paused = true)]
    async fn greet_preserves_partial_line_across_drain_timeout() {
        use tokio::io::AsyncWriteExt;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();

        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"foo\"]}\n")
                .await
                .unwrap();
            stream
                .write_all(b"{\"id\":2,\"method\":\"mining.suggest_difficulty\",")
                .await
                .unwrap();
            rx.await.unwrap();
            stream.write_all(b"\"params\":[1000]}\n").await.unwrap();
            stream
        });

        let (stream, peer) = listener.accept().await.unwrap();
        let (mut reader, prelude) = greet(framed(stream), peer).await.unwrap();

        assert_eq!(prelude.inbox.len(), 1);

        tx.send(()).unwrap();

        let line = reader.next().await.unwrap().unwrap();
        let message: Message = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            message,
            Message::Request {
                method: Method::SuggestDifficulty(_),
                ..
            }
        ));

        drop(client);
    }
}
