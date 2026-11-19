use crate::{Data, Error};

pub mod setup;

pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![setup::setup()]
}
