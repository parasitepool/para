use super::*;

mod pool;
mod user;

#[derive(Clone, Debug, Parser)]
pub struct Digest {
    #[arg(long, help = "<ENDPOINTS> to pull statistics from.")]
    pub(crate) endpoints: Vec<String>,
}

impl Digest {
    pub async fn run(&self, _options: Options, _handle: Handle) -> Result {
        todo!()
    }
}
