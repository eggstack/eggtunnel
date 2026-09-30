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
    }

    #[test]
    fn rejects_multiple_private_keys() {
        let key = rcgen::KeyPair::generate().unwrap().serialize_pem();
        let both = format!("{key}\n{key}");
        assert!(private_key(both.as_bytes()).is_err());
    }
}
