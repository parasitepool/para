use super::*;

#[derive(Clone, Debug, Parser)]
pub struct Proxy {
    #[clap(long, help = "Listen on <PROXY_URL>")]
    pub(crate) proxy_url: Option<String>,
    #[arg(long, help = "Proxy connections to <UPSTREAM_URL>")]
    pub(crate) upstream_url: Option<String>,
}

impl Proxy {
    pub async fn run(&self, handle: Handle) -> Result {
        let proxy_url = self
            .proxy_url
            .clone()
            .unwrap_or_else(|| "0.0.0.0:42069".into());

        let upstream_url = self
            .upstream_url
            .clone()
            .unwrap_or_else(|| "parasite.wtf:42069".into());

        self.spawn(handle, proxy_url.clone(), upstream_url.clone())?
            .await??;

        Ok(())
    }

    fn spawn(
        &self,
        _handle: Handle,
        proxy_url: String,
        upstream_url: String,
    ) -> Result<task::JoinHandle<io::Result<()>>> {
        Ok(tokio::spawn(async move {
            let listener = TcpListener::bind(&proxy_url).await.unwrap();

            log::info!(
                "Listening on {} and proxying to {}",
                proxy_url,
                upstream_url
            );

            loop {
                match listener.accept().await {
                    Ok((mut client, _addr)) => {
                        let upstream_url = upstream_url.clone();
                        tokio::spawn(async move {
                            let mut upstream = TcpStream::connect(upstream_url).await.unwrap();
                            log::info!("Connected to upstream for {}", client.peer_addr().unwrap());

                            let _ = copy_bidirectional(&mut client, &mut upstream)
                                .await
                                .unwrap();

                            todo!()
                        });
                    }
                    Err(_err) => {
                        // anyhow!("Accept error: {err}");
                        todo!()
                    }
                }
            }
        }))
    }
}
