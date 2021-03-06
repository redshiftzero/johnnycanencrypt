use pyo3::exceptions::*;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::types::{PyDict, PyList};
use pyo3::wrap_pyfunction;

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::Path;
use std::str;

extern crate anyhow;

extern crate sequoia_openpgp as openpgp;

use crate::openpgp::armor;
use openpgp::armor::{Kind, Writer};

use crate::openpgp::crypto::{KeyPair, SessionKey};
use crate::openpgp::parse::stream::{
    DecryptionHelper, DecryptorBuilder, DetachedVerifierBuilder, MessageLayer, MessageStructure,
    VerificationHelper,
};

use crate::openpgp::parse::Parse;
use crate::openpgp::policy::NullPolicy as NP;
use crate::openpgp::policy::Policy;
use crate::openpgp::policy::StandardPolicy as P;
use crate::openpgp::serialize::stream::{Encryptor, LiteralWriter, Message, Signer};
use crate::openpgp::serialize::Marshal;
use crate::openpgp::serialize::MarshalInto;
use crate::openpgp::types::KeyFlags;
use crate::openpgp::types::SymmetricAlgorithm;
use openpgp::cert::prelude::*;

struct Helper {
    keys: HashMap<openpgp::KeyID, KeyPair>,
}

impl Helper {
    /// Creates a Helper for the given Certs with appropriate secrets.
    fn new(p: &dyn Policy, cert: &openpgp::Cert, pass: &str) -> Self {
        // Map (sub)KeyIDs to secrets.
        let mut keys = HashMap::new();

        for ka in cert.keys().with_policy(p, None).secret() {
            keys.insert(
                ka.key().keyid(),
                ka.key()
                    .clone()
                    .decrypt_secret(&openpgp::crypto::Password::from(pass))
                    .unwrap()
                    .into_keypair()
                    .unwrap(),
            );
        }
        Helper { keys }
    }
}

impl DecryptionHelper for Helper {
    fn decrypt<D>(
        &mut self,
        pkesks: &[openpgp::packet::PKESK],
        _skesks: &[openpgp::packet::SKESK],
        sym_algo: Option<SymmetricAlgorithm>,
        mut decrypt: D,
    ) -> openpgp::Result<Option<openpgp::Fingerprint>>
    where
        D: FnMut(SymmetricAlgorithm, &SessionKey) -> bool,
    {
        // Try each PKESK until we succeed.
        for pkesk in pkesks {
            let keyid = pkesk.recipient();
            // If the keyid is not present, we should just skip to next pkesk
            let keypair = match self.keys.get_mut(&keyid) {
                Some(keypair) => keypair,
                _ => {
                    continue;
                }
            };
            let fp = keypair.public().fingerprint();
            // now get the algo
            if pkesk
                .decrypt(keypair, sym_algo)
                .map(|(algo, session_key)| decrypt(algo, &session_key))
                .unwrap_or(false)
            {
                return Ok(Some(fp));
            }
        }
        Ok(None)
    }
}

impl VerificationHelper for Helper {
    fn get_certs(&mut self, _ids: &[openpgp::KeyHandle]) -> openpgp::Result<Vec<openpgp::Cert>> {
        Ok(vec![]) // Feed the Certs to the verifier here.
    }
    fn check(&mut self, _structure: MessageStructure) -> openpgp::Result<()> {
        Ok(())
    }
}

struct VHelper {
    cert: openpgp::Cert,
}

impl VHelper {
    /// Creates a VHelper for the given Cert for signature verification.
    fn new(cert: &openpgp::Cert) -> Self {
        let cloned = cert.clone();
        VHelper { cert: cloned }
    }
}

impl VerificationHelper for VHelper {
    fn get_certs(&mut self, _ids: &[openpgp::KeyHandle]) -> openpgp::Result<Vec<openpgp::Cert>> {
        Ok(vec![self.cert.clone()]) // Feed the Certs to the verifier here.
    }
    fn check(&mut self, structure: MessageStructure) -> openpgp::Result<()> {
        let mut good = false;
        for (i, layer) in structure.into_iter().enumerate() {
            match (i, layer) {
                // First, we are interested in signatures over the
                // data, i.e. level 0 signatures.
                (0, MessageLayer::SignatureGroup { results }) => {
                    // Finally, given a VerificationResult, which only says
                    // whether the signature checks out mathematically, we apply
                    // our policy.
                    match results.into_iter().next() {
                        Some(Ok(_)) => good = true,
                        Some(Err(e)) => return Err(openpgp::Error::from(e).into()),
                        None => return Err(anyhow::anyhow!("No signature")),
                    }
                }
                _ => return Err(anyhow::anyhow!("Unexpected message structure")),
            }
        }

        if good {
            Ok(()) // Good signature.
        } else {
            Err(anyhow::anyhow!("Signature verification failed"))
        }
    }
}

// To create key pairs; from the given Cert
fn get_keys(cert: &openpgp::cert::Cert, password: String) -> Vec<openpgp::crypto::KeyPair> {
    let p = &P::new();
    let mut keys = Vec::new();
    for key in cert
        .keys()
        .with_policy(p, None)
        .alive()
        .revoked(false)
        .for_signing()
        .secret()
        .map(|kd| kd.key())
    {
        let mut key = key.clone();
        let algo = key.pk_algo();

        let _keypair = key
            .secret_mut()
            .decrypt_in_place(algo, &openpgp::crypto::Password::from(password.clone()))
            .expect("decryption failed");
        keys.push(key.into_keypair().unwrap());
    }
    keys
}

fn sign_bytes_detached_internal(
    cert: &openpgp::cert::Cert,
    input: &mut dyn io::Read,
    password: String,
) -> PyResult<String> {
    // TODO: WHY?
    let mut input = input;

    let mut keys = get_keys(cert, password);

    if keys.len() == 0 {
        return Err(PyAttributeError::new_err("No signing key is present."));
    }

    let mut result = Vec::new();
    let mut sink = armor::Writer::new(&mut result, armor::Kind::Signature)
        .expect("Failed to create armored writer.");

    // Stream an OpenPGP message.
    let message = Message::new(&mut sink);

    // Now, create a signer that emits the detached signature(s).
    let mut signer = Signer::new(message, keys.pop().expect("No key for signing"));
    for s in keys {
        signer = signer.add_signer(s);
    }
    let mut signer = signer.detached().build().expect("Failed to create signer");

    // Copy all the data.
    io::copy(&mut input, &mut signer).expect("Failed to sign data");

    // Finally, teardown the stack to ensure all the data is written.
    signer.finalize().expect("Failed to write data");

    // Finalize the armor writer.
    sink.finalize().expect("Failed to write data");

    Ok(String::from_utf8(result).unwrap())
}

#[pyfunction]
#[text_signature = "(certpath)"]
fn parse_cert_file(py: Python, certpath: String) -> PyResult<(PyObject, String, bool)> {
    let cert = openpgp::Cert::from_file(certpath).unwrap();
    let plist = PyList::empty(py);
    for ua in cert.userids() {
        let pd = PyDict::new(py);
        //println!("  {}", String::from_utf8_lossy(ua.value()));
        pd.set_item("value", String::from_utf8_lossy(ua.value()))
            .unwrap();
        // If we have a name part in the UID
        match ua.name() {
            Ok(value) => match value {
                Some(name) => {
                    pd.set_item("name", name).unwrap();
                }
                _ => (),
            },
            Err(_) => (),
        }
        // If we have a comment part in the UID
        match ua.comment() {
            Ok(value) => match value {
                Some(comment) => {
                    pd.set_item("comment", comment).unwrap();
                }
                _ => (),
            },
            Err(_) => (),
        }
        // If we have a email part in the UID
        match ua.email() {
            Ok(value) => match value {
                Some(email) => {
                    pd.set_item("email", email).unwrap();
                }
                _ => (),
            },
            Err(_) => (),
        }
        // If we have a URI part in the UID
        match ua.uri() {
            Ok(value) => match value {
                Some(uri) => {
                    pd.set_item("uri", uri).unwrap();
                }
                _ => (),
            },
            Err(_) => (),
        }
        plist.append(pd).unwrap();
    }

    Ok((plist.into(), cert.fingerprint().to_hex(), cert.is_tsk()))
}

/// This function takes a password and an userid as strings, returns a tuple of public and private
/// key and the fingerprint in hex. Remember to save the keys for future use.
#[pyfunction]
#[text_signature = "(password, userid, cipher)"]
fn create_newkey(
    password: String,
    userid: String,
    cipher: String,
) -> PyResult<(String, String, String)> {
    // Default we create RSA4k keys
    let mut ciphervalue = CipherSuite::RSA4k;
    if cipher == String::from("RSA2k") {
        ciphervalue = CipherSuite::RSA4k;
    } else if cipher == String::from("Cv25519") {
        ciphervalue = CipherSuite::Cv25519;
    }
    let (cert, _) = CertBuilder::new()
        .add_storage_encryption_subkey()
        .add_signing_subkey()
        .set_cipher_suite(ciphervalue)
        .set_password(Some(openpgp::crypto::Password::from(password)))
        .add_userid(userid)
        .generate()
        .unwrap();
    let mut buf = Vec::new();
    let mut buffer = Vec::new();

    let mut writer = Writer::new(&mut buf, Kind::SecretKey).unwrap();
    cert.as_tsk().serialize(&mut buffer).unwrap();
    writer.write_all(&buffer).unwrap();
    writer.finalize().unwrap();
    let armored = cert.armored().to_vec().unwrap();
    Ok((
        String::from_utf8(armored).unwrap(),
        String::from_utf8(buf).unwrap(),
        cert.fingerprint().to_hex(),
    ))
}

/// This function takes a list of public key paths, and encrypts the given data in bytes to an output
/// file. You can also pass boolen flag armor for armored output.
#[pyfunction]
#[text_signature = "(publickeys, data, output, armor=False)"]
fn encrypt_bytes_to_file(
    publickeys: Vec<String>,
    data: Vec<u8>,
    output: Vec<u8>,
    armor: Option<bool>,
) -> PyResult<bool> {
    let mut certs = Vec::new();
    for fpath in publickeys {
        if !std::fs::metadata(fpath.clone()).is_ok() {
            return Err(PyFileNotFoundError::new_err(format!(
                "{} is not found.",
                fpath
            )));
        }
        certs.push(openpgp::Cert::from_file(&fpath).unwrap());
    }
    let mode = KeyFlags::default().set_storage_encryption(true);

    let p = &P::new();
    let recipients = certs.iter().flat_map(|cert| {
        cert.keys()
            .with_policy(p, None)
            .alive()
            .revoked(false)
            .key_flags(&mode)
    });
    let mut outfile = File::create(str::from_utf8(&output[..]).unwrap()).unwrap();
    // TODO: Find better ways to write this code
    match armor {
        // For armored output file.
        Some(true) => {
            let mut sink = armor::Writer::new(&mut outfile, armor::Kind::Message).unwrap();
            // Stream an OpenPGP message.
            let message = Message::new(&mut sink);

            // We want to encrypt a literal data packet.
            let encryptor = Encryptor::for_recipients(message, recipients)
                .build()
                .expect("Failed to create encryptor");

            let mut literal_writer = LiteralWriter::new(encryptor)
                .build()
                .expect("Failed to create literal writer");

            // Copy data to our writer stack to encrypt the data.
            literal_writer.write_all(&data).unwrap();

            // Finally, finalize the OpenPGP message by tearing down the
            // writer stack.
            literal_writer.finalize().unwrap();

            // Finalize the armor writer.
            sink.finalize().expect("Failed to write data");
        }
        _ => {
            let message = Message::new(&mut outfile);

            // We want to encrypt a literal data packet.
            let encryptor = Encryptor::for_recipients(message, recipients)
                .build()
                .expect("Failed to create encryptor");

            let mut literal_writer = LiteralWriter::new(encryptor)
                .build()
                .expect("Failed to create literal writer");

            // Copy data to our writer stack to encrypt the data.
            literal_writer.write_all(&data).unwrap();

            // Finally, finalize the OpenPGP message by tearing down the
            // writer stack.
            literal_writer.finalize().unwrap();
        }
    }

    Ok(true)
}

/// This function takes a list of public key paths, and encrypts the given filepath to an output
/// file. You can also pass boolen flag armor for armored output.
#[pyfunction]
#[text_signature = "(publickeys, filepath, output, armor=False)"]
fn encrypt_file_internal(
    publickeys: Vec<String>,
    filepath: Vec<u8>,
    output: Vec<u8>,
    armor: Option<bool>,
) -> PyResult<bool> {
    let mut certs = Vec::new();
    for fpath in publickeys {
        if !std::fs::metadata(fpath.clone()).is_ok() {
            return Err(PyFileNotFoundError::new_err(format!(
                "{} is not found.",
                fpath
            )));
        }
        certs.push(openpgp::Cert::from_file(&fpath).unwrap());
    }
    let mode = KeyFlags::default().set_storage_encryption(true);

    let p = &P::new();
    let recipients = certs.iter().flat_map(|cert| {
        cert.keys()
            .with_policy(p, None)
            .alive()
            .revoked(false)
            .key_flags(&mode)
    });

    let mut input = File::open(str::from_utf8(&filepath[..]).unwrap()).unwrap();
    let mut outfile = File::create(str::from_utf8(&output[..]).unwrap()).unwrap();
    // TODO: Find better ways to write this code
    match armor {
        // For armored output file.
        Some(true) => {
            let mut sink = armor::Writer::new(&mut outfile, armor::Kind::Message).unwrap();
            // Stream an OpenPGP message.
            let message = Message::new(&mut sink);

            // We want to encrypt a literal data packet.
            let encryptor = Encryptor::for_recipients(message, recipients)
                .build()
                .expect("Failed to create encryptor");

            let mut literal_writer = LiteralWriter::new(encryptor)
                .build()
                .expect("Failed to create literal writer");

            // Copy stdin to our writer stack to encrypt the data.
            io::copy(&mut input, &mut literal_writer).expect("Failed to encrypt");
            //literal_writer.write_all(&data).unwrap();

            // Finally, finalize the OpenPGP message by tearing down the
            // writer stack.
            literal_writer.finalize().unwrap();

            // Finalize the armor writer.
            sink.finalize().expect("Failed to write data");
        }
        _ => {
            let message = Message::new(&mut outfile);

            // We want to encrypt a literal data packet.
            let encryptor = Encryptor::for_recipients(message, recipients)
                .build()
                .expect("Failed to create encryptor");

            let mut literal_writer = LiteralWriter::new(encryptor)
                .build()
                .expect("Failed to create literal writer");

            // Copy stdin to our writer stack to encrypt the data.
            io::copy(&mut input, &mut literal_writer).expect("Failed to encrypt");
            //literal_writer.write_all(&data).unwrap();

            // Finally, finalize the OpenPGP message by tearing down the
            // writer stack.
            literal_writer.finalize().unwrap();
        }
    }

    Ok(true)
}

/// This function takes a list of public key paths, and encrypts the given data in bytes and returns it.
/// You can also pass boolen flag armor for armored output.
#[pyfunction]
#[text_signature = "(publickeys, data, armor=False)"]
fn encrypt_bytes_to_bytes(
    py: Python,
    publickeys: Vec<String>,
    data: Vec<u8>,
    armor: Option<bool>,
) -> PyResult<PyObject> {
    let mut certs = Vec::new();
    for fpath in publickeys {
        if !std::fs::metadata(fpath.clone()).is_ok() {
            return Err(PyFileNotFoundError::new_err(format!(
                "{} is not found.",
                fpath
            )));
        }
        certs.push(openpgp::Cert::from_file(&fpath).unwrap());
    }
    let mode = KeyFlags::default().set_storage_encryption(true);

    let p = &P::new();
    let recipients = certs.iter().flat_map(|cert| {
        cert.keys()
            .with_policy(p, None)
            .alive()
            .revoked(false)
            .key_flags(&mode)
    });
    // TODO: Find better way to do this in rust
    let mut result = Vec::new();
    let mut result2 = Vec::new();
    let mut sink = armor::Writer::new(&mut result2, armor::Kind::Message)?;
    // Stream an OpenPGP message.
    let message = match armor {
        Some(true) => Message::new(&mut sink),
        _ => Message::new(&mut result),
    };
    // We want to encrypt a literal data packet.
    let encryptor = Encryptor::for_recipients(message, recipients)
        .build()
        .expect("Failed to create encryptor");

    let mut literal_writer = LiteralWriter::new(encryptor)
        .build()
        .expect("Failed to create literal writer");

    // Copy stdin to our writer stack to encrypt the data.
    // io::copy(&mut data, &mut literal_writer).expect("Failed to encrypt");
    literal_writer.write_all(&data).unwrap();

    // Finally, finalize the OpenPGP message by tearing down the
    // writer stack.
    literal_writer.finalize().unwrap();

    match armor {
        Some(true) => {
            // Finalize the armor writer.
            sink.finalize().expect("Failed to write data");
            let res = PyBytes::new(py, &result2);
            return Ok(res.into());
        }
        _ => {
            let res = PyBytes::new(py, &result);
            return Ok(res.into());
        }
    }
}

#[pyclass]
#[derive(Debug)]
struct Johnny {
    #[pyo3(get, set)]
    filepath: String,
    cert: openpgp::cert::Cert,
}

#[pymethods]
impl Johnny {
    #[new]
    fn new(filepath: String) -> PyResult<Self> {
        if !std::fs::metadata(filepath.clone()).is_ok() {
            return Err(PyFileNotFoundError::new_err(format!(
                "{} is not found.",
                filepath
            )));
        }
        let cert = openpgp::Cert::from_file(&filepath).unwrap();
        Ok(Johnny { filepath, cert })
    }

    pub fn encrypt_bytes(
        &self,
        py: Python,
        data: Vec<u8>,
        armor: Option<bool>,
    ) -> PyResult<PyObject> {
        let mode = KeyFlags::default().set_storage_encryption(true);
        let p = &P::new();
        let recipients = self
            .cert
            .keys()
            .with_policy(p, None)
            .alive()
            .revoked(false)
            .key_flags(&mode);
        // TODO: Find better way to do this in rust
        let mut result = Vec::new();
        let mut result2 = Vec::new();
        let mut sink = armor::Writer::new(&mut result2, armor::Kind::Message)?;
        // Stream an OpenPGP message.
        let message = match armor {
            Some(true) => Message::new(&mut sink),
            _ => Message::new(&mut result),
        };
        // We want to encrypt a literal data packet.
        let encryptor = Encryptor::for_recipients(message, recipients)
            .build()
            .expect("Failed to create encryptor");

        let mut literal_writer = LiteralWriter::new(encryptor)
            .build()
            .expect("Failed to create literal writer");

        // Copy stdin to our writer stack to encrypt the data.
        // io::copy(&mut data, &mut literal_writer).expect("Failed to encrypt");
        literal_writer.write_all(&data).unwrap();

        // Finally, finalize the OpenPGP message by tearing down the
        // writer stack.
        literal_writer.finalize().unwrap();

        match armor {
            Some(true) => {
                // Finalize the armor writer.
                sink.finalize().expect("Failed to write data");
                let res = PyBytes::new(py, &result2);
                return Ok(res.into());
            }
            _ => {
                let res = PyBytes::new(py, &result);
                return Ok(res.into());
            }
        }
    }

    pub fn decrypt_bytes(&self, py: Python, data: Vec<u8>, password: String) -> PyResult<PyObject> {
        let p = &NP::new();

        let mut result = Vec::new();
        let reader = std::io::BufReader::new(&data[..]);

        let dec = DecryptorBuilder::from_reader(reader);
        let dec2 = match dec {
            Ok(dec) => dec,
            Err(msg) => panic!(msg),
        };
        let mut decryptor = match dec2.with_policy(p, None, Helper::new(p, &self.cert, &password)) {
            Ok(decr) => decr,
            Err(msg) => panic!(msg),
        };
        std::io::copy(&mut decryptor, &mut result).unwrap();
        let res = PyBytes::new(py, &result);
        Ok(res.into())
    }
    pub fn encrypt_file(
        &self,
        filepath: Vec<u8>,
        output: Vec<u8>,
        armor: Option<bool>,
    ) -> PyResult<bool> {
        let mode = KeyFlags::default().set_storage_encryption(true);
        let p = &P::new();
        let recipients = self
            .cert
            .keys()
            .with_policy(p, None)
            .alive()
            .revoked(false)
            .key_flags(&mode);
        let mut input = File::open(str::from_utf8(&filepath[..]).unwrap()).unwrap();
        let mut outfile = File::create(str::from_utf8(&output[..]).unwrap()).unwrap();
        // TODO: Find better ways to write this code
        match armor {
            // For armored output file.
            Some(true) => {
                let mut sink = armor::Writer::new(&mut outfile, armor::Kind::Message).unwrap();
                // Stream an OpenPGP message.
                let message = Message::new(&mut sink);

                // We want to encrypt a literal data packet.
                let encryptor = Encryptor::for_recipients(message, recipients)
                    .build()
                    .expect("Failed to create encryptor");

                let mut literal_writer = LiteralWriter::new(encryptor)
                    .build()
                    .expect("Failed to create literal writer");

                // Copy stdin to our writer stack to encrypt the data.
                io::copy(&mut input, &mut literal_writer).expect("Failed to encrypt");
                //literal_writer.write_all(&data).unwrap();

                // Finally, finalize the OpenPGP message by tearing down the
                // writer stack.
                literal_writer.finalize().unwrap();

                // Finalize the armor writer.
                sink.finalize().expect("Failed to write data");
            }
            _ => {
                let message = Message::new(&mut outfile);

                // We want to encrypt a literal data packet.
                let encryptor = Encryptor::for_recipients(message, recipients)
                    .build()
                    .expect("Failed to create encryptor");

                let mut literal_writer = LiteralWriter::new(encryptor)
                    .build()
                    .expect("Failed to create literal writer");

                // Copy stdin to our writer stack to encrypt the data.
                io::copy(&mut input, &mut literal_writer).expect("Failed to encrypt");
                //literal_writer.write_all(&data).unwrap();

                // Finally, finalize the OpenPGP message by tearing down the
                // writer stack.
                literal_writer.finalize().unwrap();
            }
        }

        Ok(true)
    }

    pub fn decrypt_file(
        &self,
        filepath: Vec<u8>,
        output: Vec<u8>,
        password: String,
    ) -> PyResult<bool> {
        let p = &NP::new();

        let input = File::open(str::from_utf8(&filepath[..]).unwrap()).unwrap();
        let mut outfile = File::create(str::from_utf8(&output[..]).unwrap()).unwrap();

        let mut decryptor = DecryptorBuilder::from_reader(input)
            .unwrap()
            .with_policy(p, None, Helper::new(p, &self.cert, &password))
            .unwrap();
        std::io::copy(&mut decryptor, &mut outfile).unwrap();
        Ok(true)
    }

    pub fn sign_bytes_detached(&self, data: Vec<u8>, password: String) -> PyResult<String> {
        let mut localdata = io::Cursor::new(data);
        sign_bytes_detached_internal(&self.cert, &mut localdata, password)
    }

    pub fn sign_file_detached(&self, filepath: Vec<u8>, password: String) -> PyResult<String> {
        let file = Path::new(str::from_utf8(&filepath[..]).unwrap());
        let mut localdata = File::open(file).unwrap();
        sign_bytes_detached_internal(&self.cert, &mut localdata, password)
    }

    pub fn verify_bytes(&self, data: Vec<u8>, sig: Vec<u8>) -> PyResult<bool> {
        let p = &P::new();
        let vh = VHelper::new(&self.cert);
        let mut v = DetachedVerifierBuilder::from_bytes(&sig[..])
            .unwrap()
            .with_policy(p, None, vh)
            .unwrap();
        match v.verify_bytes(data) {
            Ok(()) => return Ok(true),
            Err(_) => return Ok(false),
        };
    }
    pub fn verify_file(&self, filepath: Vec<u8>, sig: Vec<u8>) -> PyResult<bool> {
        let p = &P::new();
        let vh = VHelper::new(&self.cert);
        let mut v = DetachedVerifierBuilder::from_bytes(&sig[..])
            .unwrap()
            .with_policy(p, None, vh)
            .unwrap();
        let path = Path::new(str::from_utf8(&filepath[..]).unwrap());
        match v.verify_file(path) {
            Ok(()) => return Ok(true),
            Err(_) => return Ok(false),
        };
    }
}

#[pymodule]
/// A Python module implemented in Rust.
fn johnnycanencrypt(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_wrapped(wrap_pyfunction!(create_newkey))?;
    m.add_wrapped(wrap_pyfunction!(parse_cert_file))?;
    m.add_wrapped(wrap_pyfunction!(encrypt_bytes_to_file))?;
    m.add_wrapped(wrap_pyfunction!(encrypt_bytes_to_bytes))?;
    m.add_wrapped(wrap_pyfunction!(encrypt_file_internal))?;
    m.add_class::<Johnny>()?;
    Ok(())
}
