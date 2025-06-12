use {super::*, crate::client::Client};

mod hasher;

#[derive(Debug, Parser)]
pub(crate) struct Miner {
    #[arg(long, help = "Stratum <HOST>")]
    host: String,
    #[arg(long, help = "Stratum <PORT>")]
    port: u16,
    #[arg(long, help = "Stratum <USER>")]
    user: String,
    #[arg(long, help = "Stratum <PASSWORD>")]
    password: String,
}

impl Miner {
    pub(crate) async fn run(&self) -> Result {
        let mut client = Client::connect(&self.host, self.port, &self.user, &self.password).await?;

        client.subscribe().await?;
        client.authorize().await?;
        
        // handle notifications
        // handle requests
        // ignore responses that do not have corresponding request

        loop {
            tokio::select! {
                Some(msg) = client.message_receiver.recv() => {
                    match msg {
                        Message::Notification { method, params } => {
                            match method.as_str() {
                                "mining.notify" => {
                                    let notify: Notify = serde_json::from_value(params)?;
                                    log::info!("Got new job: {:?}", notify);
                                }
                                "mining.set_difficulty" => {
                                    let set_difficulty: SetDifficulty = serde_json::from_value(params)?;
                                    log::info!("Got new set difficulty: {:?}", set_difficulty.0[0]);
                                },
                                _ => log::info!("Unhandled notification method: {}", method)
                            }
                        }
                        Message::Response { id, result, error } => {
                            log::info!("Response to id={id}: result={result:?}, error={error:?}");
                        }
                        _ => {
                            log::info!("Unhandled message: {:?}", msg);
                        }
                    }
                }
                _ = ctrl_c() => {
                    log::info!("Shutting down");
                    client.shutdown();
                    break;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_miner_args(args: &str) -> Miner {
        match Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                Subcommand::Miner(miner) => miner,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    #[test]
    fn parse_args() {
        parse_miner_args(
            "para miner --host parasite.wtf --port 42069 --user bc1q8jx6g9ujlqmdx3jnt3ap6ll2fdwqjdkdgs959m.worker1.aed48ef@parasite.sati.pro --password x",
        );
    }
}

//   let job_id = 123;
//   let target = target(4);

//   println!(
//       "Mining...\nId\t\t{}\nTarget\t\t{}\nDifficulty\t{}\n\n",
//       job_id,
//       target,
//       target.difficulty_float()
//   );

//   let mut hasher = Hasher {
//       header: header(None, None),
//       target,
//   };

//   let start = Instant::now();
//   let header = hasher.hash()?;
//   let duration = (Instant::now() - start).as_millis();

//   if header.validate_pow(header.target()).is_ok() {
//       println!("Block found!");
//   } else {
//       println!("Share found!");
//   }

//   println!(
//       "Nonce\t\t{}\nTime\t\t{}ms\nBlockhash\t{}\nTarget\t\t{}\nWork\t\t{}\n",
//       header.nonce,
//       duration,
//       header.block_hash(),
//       target_as_block_hash(target),
//       target.to_work(),
//   );
//
//    fn target_as_block_hash(target: Target) -> BlockHash {
//        BlockHash::from_raw_hash(Hash::from_byte_array(target.to_le_bytes()))
//    }
