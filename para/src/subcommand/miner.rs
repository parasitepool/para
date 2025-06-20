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

        let subscribe = client.subscribe().await?;
        info!("Subscribed successfully: {subscribe}");
        // TODO: set extranonce etc.

        client.authorize().await?;
        info!("Authorized successfully");

        let mut pool_difficulty = Difficulty::default();

        loop {
            tokio::select! {
                Some(msg) = client.notifications.recv() => {
                    if let Message::Notification { method, params } = msg {
                        match method.as_str() {
                            "mining.notify" => {
                                let notify: Notify = serde_json::from_value(dbg!(params))?;
                                let mut hasher = hasher::Hasher {
                                    header: Header {
                                        version: Version::TWO,
                                        prev_blockhash: notify.prevhash,
                                        merkle_root: TxMerkleNode::from_raw_hash(BlockHash::all_zeros().to_raw_hash()),
                                        time: u32::from_str_radix(&notify.ntime, 16)?,
                                        bits: CompactTarget::from_unprefixed_hex(&notify.nbits)?,
                                        nonce: 0,
                                    },
                                    pool_target: pool_difficulty.to_target(),
                                };

                                dbg!(hasher.hash()?);

                                dbg!(&hasher);

                            }
                            "mining.set_difficulty" => {
                                let pool_difficulty =  serde_json::from_value::<SetDifficulty>(params)?.to_difficulty();

                                let pool_target = pool_difficulty.to_target();

                                info!("Pool difficulty: {:?}", pool_difficulty);
                                info!("Pool target: {:?}", pool_target);
                                info!("Pool target (nbits): {:?}", pool_target.to_compact_lossy());
                            }
                            _ => warn!("Unhandled notification: {}", method),
                        }
                    }
                }
                Some(msg) = client.requests.recv() => {
                    if let Message::Request { method, params, id } = msg {
                        info!("Got request method={method} with id={id} with params={params}");
                    }
                }
                _ = ctrl_c() => {
                    info!("Shutting down");
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
