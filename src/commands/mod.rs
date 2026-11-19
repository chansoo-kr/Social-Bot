use crate::{Data, Error};

pub mod setup;
pub mod verification;

pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![setup::setup(), verification::verification()]
}
