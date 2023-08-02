use ic_agent::{export::Principal, identity::BasicIdentity, Identity};
use ring::signature::Ed25519KeyPair;
use std::{fs, path::Path};

pub fn load_key_pair(dir: &str) -> Result<Ed25519KeyPair, String> {
    let key_pair;
    if !Path::new(dir).is_file() {
        let rng = ring::rand::SystemRandom::new();
        key_pair = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|e| format!("Could not generate a key pair. Error: {:?}", e))?
            .as_ref()
            .to_vec();
        // TODO: print out seed phrase
        fs::write(dir, &key_pair)
            .map_err(|e| format!("Could not write to file. Error: {}", e.to_string()))?;
    } else {
        key_pair = fs::read(dir)
            .map_err(|e| format!("Could not read from file. Error: {}", e.to_string()))?;
    }
    Ed25519KeyPair::from_pkcs8(&key_pair)
        .map_err(|e| format!("Could not parse the key pair. Error: {}", e.to_string()))
}

pub fn get_identity_from_key_pair(key_pair: Ed25519KeyPair) -> BasicIdentity {
    BasicIdentity::from_key_pair(key_pair)
}

pub fn get_principal_from_identity(identity: BasicIdentity) -> Result<Principal, String> {
    identity
        .sender()
        .map_err(|e| format!("Could not get principal from key pair. Error: {:?}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_identity_from_key_pair() {
        let key_pair = load_key_pair("./tests/data/test_key_pair").unwrap();
        assert!(std::panic::catch_unwind(|| get_identity_from_key_pair(key_pair)).is_ok());
    }

    #[test]
    fn test_get_identity_from_wrong_key_pair() {
        let key_pair = load_key_pair("./tests/data/wrong_key_pair");
        assert_eq!(
            key_pair.unwrap_err(),
            String::from("Could not parse the key pair. Error: InvalidEncoding")
        );
    }

    #[test]
    fn test_get_principal_from_identity() {
        let key_pair = load_key_pair("./tests/data/test_key_pair").unwrap();
        let identity = get_identity_from_key_pair(key_pair);
        let principal = get_principal_from_identity(identity).unwrap();
        assert_eq!(
            principal.to_string(),
            String::from("7cio4-7j2lx-6f3tp-mkfw7-t4amd-tjphs-hkits-6qa7x-hmnmx-yvwxk-nqe")
        );
    }
}
