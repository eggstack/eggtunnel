//! PEM parsing backed by the maintained parser shipped with Rustls.

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

pub(crate) fn certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, ()> {
    // Bound the chain to remove the unbounded-`Vec` OOM vector on huge
    // local PEM inputs. `private_key` already rejects `>1` key.
    const MAX_CERTS: usize = 32;
    let certificates = CertificateDer::pem_slice_iter(pem)
        .take(MAX_CERTS + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ())?;
    if certificates.is_empty() || certificates.len() > MAX_CERTS {
        return Err(());
    }
    Ok(certificates)
}

pub(crate) fn private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, ()> {
    if pem.len() > eggtunnel_proto::MAX_FRAME_BYTES {
        return Err(());
    }
    let mut keys = PrivateKeyDer::pem_slice_iter(pem);
    let key = keys.next().ok_or(())?.map_err(|_| ())?;
    if keys.next().is_some() {
        return Err(());
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_malformed_pem() {
        assert!(certificates(b"").is_err());
        assert!(
            certificates(b"-----BEGIN CERTIFICATE-----\n!\n-----END CERTIFICATE-----").is_err()
        );
        assert!(private_key(b"").is_err());
        assert!(private_key(b"-----BEGIN PRIVATE KEY-----\n!\n-----END PRIVATE KEY-----").is_err());
        assert!(private_key(&vec![0; eggtunnel_proto::MAX_FRAME_BYTES + 1]).is_err());
    }

    #[test]
    fn rejects_multiple_private_keys() {
        let key = rcgen::KeyPair::generate().unwrap().serialize_pem();
        let both = format!("{key}\n{key}");
        assert!(private_key(both.as_bytes()).is_err());
    }

    #[test]
    fn certificate_chain_limit_is_inclusive_and_mixed_bundles_are_supported() {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let cert = generated.cert.pem();
        let key = generated.key_pair.serialize_pem();
        assert_eq!(certificates(cert.repeat(32).as_bytes()).unwrap().len(), 32);
        assert!(certificates(cert.repeat(33).as_bytes()).is_err());
        let bundle = format!("{key}\n{cert}");
        assert_eq!(certificates(bundle.as_bytes()).unwrap().len(), 1);
        assert!(private_key(bundle.as_bytes()).is_ok());
    }
}
