use ic_agent::{export::Principal, identity::BasicIdentity, Identity};
use ring::signature::Ed25519KeyPair;
use std::{fs, path::Path};

pub fn load_key_pair(dir: &str) -> Ed25519KeyPair {
    if !Path::new(dir).is_file() {
        let rng = ring::rand::SystemRandom::new();
        let key_pair =
            Ed25519KeyPair::generate_pkcs8(&rng).expect("Could not generate a key pair.");
        // TODO: print out seed phrase
        fs::write(dir, key_pair.as_ref()).unwrap();
        Ed25519KeyPair::from_pkcs8(key_pair.as_ref()).expect("Could not read the key pair.")
    } else {
        let key_pair = fs::read(dir).unwrap();
        Ed25519KeyPair::from_pkcs8(&key_pair).expect("Could not read the key pair.")
    }
}

pub fn get_identity_from_key_pair(key_pair: Ed25519KeyPair) -> BasicIdentity {
    BasicIdentity::from_key_pair(key_pair)
}

pub fn get_principal_from_identity(identity: BasicIdentity) -> Principal {
    identity
        .sender()
        .expect("Could not get principal from key pair.")
}
