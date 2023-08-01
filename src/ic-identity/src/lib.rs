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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_identity_from_key_pair() {
        let key_pair = load_key_pair("./tests/data/test_key_pair");
        assert!(std::panic::catch_unwind(|| get_identity_from_key_pair(key_pair)).is_ok());
    }

    #[test]
    #[should_panic(expected = "Could not read the key pair.: KeyRejected(\"InvalidEncoding\")")]
    fn test_get_identity_from_wrong_key_pair() {
        let key_pair = load_key_pair("./tests/data/wrong_key_pair");
        get_identity_from_key_pair(key_pair);
    }

    #[test]
    fn test_get_principal_from_identity() {
        let key_pair = load_key_pair("./tests/data/test_key_pair");
        let identity = get_identity_from_key_pair(key_pair);
        let principal = get_principal_from_identity(identity);
        assert_eq!(
            principal.to_string(),
            String::from("7cio4-7j2lx-6f3tp-mkfw7-t4amd-tjphs-hkits-6qa7x-hmnmx-yvwxk-nqe")
        );
    }
}
