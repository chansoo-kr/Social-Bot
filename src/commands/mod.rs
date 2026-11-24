use crate::{Data, Error};

pub mod setup;
pub mod util;
pub mod verification;

pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![setup::setup(), util::util(), verification::verification()]
}
