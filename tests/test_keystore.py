import os
import shutil
import tempfile
import johnnycanencrypt as jce
import pytest

from .utils import clean_outputfiles, verify_files


DATA = "Kushal loves 🦀"


def setup_module(module):
    module.tmpdirname = tempfile.TemporaryDirectory()


def teardown_module(module):
    del module.tmpdirname


def test_correct_keystore_path():
    ks = jce.KeyStore("tests/files/store")


def test_nonexisting_keystore_path():
    with pytest.raises(OSError):
        ks = jce.KeyStore("tests/files2/")


def test_no_such_key():
    with pytest.raises(jce.KeyNotFoundError):
        ks = jce.KeyStore("tests/files/store")
        key = ks.get_key("BB2D3F20233286371C3123D5209940B9669ED677")


def test_keystore_lifecycle():
    ks = jce.KeyStore(tmpdirname.name)
    newkey = ks.create_newkey("redhat", "test key1 <email@example.com>", "RSA4k")
    # the default key must be of public
    assert newkey.keytype == "public"
    fingerprint = newkey.fingerprint
    # Keys should be on disk
    assert os.path.exists(os.path.join(tmpdirname.name, f"{fingerprint}.pub"))
    assert os.path.exists(os.path.join(tmpdirname.name, f"{fingerprint}.sec"))

    # Get the key from disk

    k = ks.get_key(fingerprint)
    assert k.keytype == "public"
    assert k.keypath == newkey.keypath
    ks.import_cert("tests/files/store/public.asc")
    ks.import_cert("tests/files/store/pgp_keys.asc")
    ks.import_cert("tests/files/store/hellopublic.asc")
    ks.import_cert("tests/files/store/secret.asc")
    # Now check the numbers of keys in the store
    assert (4, 2) == ks.details()

    ks.delete_key("BB2D3F20233286371C3123D5209940B9669ED621")
    assert (3, 1) == ks.details()

    # Now verify email cache
    key_via_fingerprint = ks.get_key("A85FF376759C994A8A1168D8D8219C8C43F6C5E1")
    keys_via_emails = ks.get_keys(email="kushaldas@gmail.com")
    assert len(keys_via_emails) == 1
    assert key_via_fingerprint == keys_via_emails[0]

    # Now verify name cache
    key_via_fingerprint = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    keys_via_names = ks.get_keys(name="test key")
    assert len(keys_via_names) == 1
    assert key_via_fingerprint == keys_via_names[0]


def test_keystore_contains_key():
    "verifies __contains__ method for keystore"
    ks = jce.KeyStore(tmpdirname.name)
    keypath = "tests/files/store/secret.asc"
    ks.import_cert(keypath)
    _, fingerprint, keytype = jce.parse_cert_file(keypath)
    k = jce.Key(keypath, fingerprint, keytype)

    # First only the fingerprint
    assert fingerprint in ks
    # Next the Key object
    assert k in ks
    # This should be false
    assert not "1111111" in ks


def test_keystore_details():
    ks = jce.KeyStore("./tests/files/store")
    assert (4, 2) == ks.details()


def test_key_deletion():
    tempdir = tempfile.TemporaryDirectory()
    ks = jce.KeyStore(tempdir.name)
    ks.import_cert("tests/files/store/public.asc")
    ks.import_cert("tests/files/store/pgp_keys.asc")
    ks.import_cert("tests/files/store/hellopublic.asc")
    ks.import_cert("tests/files/store/hellosecret.asc")
    ks.import_cert("tests/files/store/secret.asc")
    assert (3, 2) == ks.details()

    assert ks.get_keys(name="Test user")
    assert ks.get_keys(email="test@gmail.com")
    # Now let us delete one public key
    filepath = os.path.join(
        tempdir.name, "BB2D3F20233286371C3123D5209940B9669ED621.pub"
    )
    assert os.path.exists(filepath)
    ks.delete_key("BB2D3F20233286371C3123D5209940B9669ED621", whichkey="public")
    assert (2, 2) == ks.details()
    assert not os.path.exists(filepath)

    # The secret file should exists
    filepath = os.path.join(
        tempdir.name, "BB2D3F20233286371C3123D5209940B9669ED621.sec"
    )
    assert os.path.exists(filepath)
    # Now let us delete one secret key
    ks.delete_key("BB2D3F20233286371C3123D5209940B9669ED621", whichkey="secret")
    assert (2, 1) == ks.details()
    assert not os.path.exists(filepath)

    ks.delete_key("BB2D3F20233286371C3123D5209940B9669ED621", whichkey="both")
    assert not ks.get_keys(name="Test user")
    assert not ks.get_keys(email="test@gmail.com")

    # Now delete both public and secret
    assert ks.get_keys(name="test key")
    ks.delete_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    assert (1, 0) == ks.details()
    for extension in ["pub", "sec"]:
        filepath = os.path.join(
            tempdir.name, f"6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99.{extension}"
        )
        assert not os.path.exists(filepath)

    # Deletion should also remove from all caches
    assert not ks.get_keys(name="test key")


def test_key_equality():
    ks = jce.KeyStore("tests/files/store")
    key_from_store = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    key_from_disk = jce.Key(
        "./tests/files/store/hellopublic.asc",
        "6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99",
        "public",
    )
    assert key_from_store == key_from_disk


def test_key_inequality():
    "public key and secret key are not equal"
    ks = jce.KeyStore("tests/files/store")
    key_from_store = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    key_from_store2 = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99", "secret")
    assert not key_from_store == key_from_store2


def test_ks_encrypt_decrypt_bytes():
    "Encrypts and decrypt some bytes"
    ks = jce.KeyStore("tests/files/store")
    public_key = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    encrypted = ks.encrypt(public_key, DATA)
    assert encrypted.startswith(b"-----BEGIN PGP MESSAGE-----\n")
    secret_key = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99", "secret")
    decrypted_text = ks.decrypt(secret_key, encrypted, password="redhat").decode(
        "utf-8"
    )
    assert DATA == decrypted_text


def test_ks_encrypt_decrypt_bytes_multiple_recipients():
    "Encrypts and decrypt some bytes"
    ks = jce.KeyStore("tests/files/store")
    key1 = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    key2 = ks.get_key("BB2D3F20233286371C3123D5209940B9669ED621")
    encrypted = ks.encrypt([key1, key2], DATA)
    assert encrypted.startswith(b"-----BEGIN PGP MESSAGE-----\n")
    secret_key1 = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99", "secret")
    decrypted_text = ks.decrypt(secret_key1, encrypted, password="redhat").decode(
        "utf-8"
    )
    assert DATA == decrypted_text
    secret_key2 = ks.get_key("BB2D3F20233286371C3123D5209940B9669ED621", "secret")
    decrypted_text = ks.decrypt(secret_key2, encrypted, password="redhat").decode(
        "utf-8"
    )
    assert DATA == decrypted_text


def test_ks_encrypt_decrypt_bytes_to_file():
    "Encrypts and decrypt some bytes"
    outputfile = os.path.join(tmpdirname.name, "encrypted.asc")
    ks = jce.KeyStore("tests/files/store")
    secret_key = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    assert ks.encrypt(secret_key, DATA, outputfile=outputfile)
    with open(outputfile, "rb") as fobj:
        encrypted = fobj.read()
    secret_key = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99", "secret")
    decrypted_text = ks.decrypt(secret_key, encrypted, password="redhat").decode(
        "utf-8"
    )
    assert DATA == decrypted_text


def test_ks_encrypt_decrypt_bytes_to_file_multiple_recipients():
    "Encrypts and decrypt some bytes"
    outputfile = os.path.join(tmpdirname.name, "encrypted.asc")
    ks = jce.KeyStore("tests/files/store")
    key1 = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    key2 = ks.get_key("BB2D3F20233286371C3123D5209940B9669ED621")
    assert ks.encrypt([key1, key2], DATA, outputfile=outputfile)
    with open(outputfile, "rb") as fobj:
        encrypted = fobj.read()
    secret_key = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99", "secret")
    decrypted_text = ks.decrypt(secret_key, encrypted, password="redhat").decode(
        "utf-8"
    )
    assert DATA == decrypted_text


def test_ks_encrypt_decrypt_file(encrypt_decrypt_file):
    "Encrypts and decrypt some bytes"
    inputfile = "tests/files/text.txt"
    output = "/tmp/text-encrypted.pgp"
    decrypted_output = "/tmp/text.txt"

    ks = jce.KeyStore("tests/files/store")
    public_key = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    assert ks.encrypt_file(public_key, inputfile, output)
    secret_key = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99", "secret")
    ks.decrypt_file(secret_key, output, decrypted_output, password="redhat")
    verify_files(inputfile, decrypted_output)


def test_ks_encrypt_decrypt_file_multiple_recipients(encrypt_decrypt_file):
    "Encrypts and decrypt some bytes"
    inputfile = "tests/files/text.txt"
    output = "/tmp/text-encrypted.pgp"
    decrypted_output = "/tmp/text.txt"

    ks = jce.KeyStore("tests/files/store")
    key1 = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99")
    key2 = ks.get_key("BB2D3F20233286371C3123D5209940B9669ED621")
    encrypted = ks.encrypt_file([key1, key2], inputfile, output)
    secret_key1 = ks.get_key("6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99", "secret")
    ks.decrypt_file(secret_key1, output, decrypted_output, password="redhat")
    verify_files(inputfile, decrypted_output)
    secret_key2 = ks.get_key("BB2D3F20233286371C3123D5209940B9669ED621", "secret")
    ks.decrypt_file(secret_key2, output, decrypted_output, password="redhat")
    verify_files(inputfile, decrypted_output)


def test_ks_sign_data():
    ks = jce.KeyStore("tests/files/store")
    key = "6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99"
    signed = ks.sign(key, "hello", "redhat")
    assert signed.startswith("-----BEGIN PGP SIGNATURE-----\n")
    assert ks.verify(key, "hello", signed)


def test_ks_sign_data_fails():
    ks = jce.KeyStore("tests/files/store")
    key = "6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99"
    signed = ks.sign(key, "hello", "redhat")
    assert signed.startswith("-----BEGIN PGP SIGNATURE-----\n")
    assert not ks.verify(key, "hello2", signed)


def test_ks_sign_verify_file():
    inputfile = "tests/files/text.txt"
    tempdir = tempfile.TemporaryDirectory()
    shutil.copy(inputfile, tempdir.name)
    ks = jce.KeyStore("tests/files/store")
    key = "6AC6957E2589CB8B5221F6508ADA07F0A0F7BA99"
    file_to_be_signed = os.path.join(tempdir.name, "text.txt")
    signed = ks.sign_file(key, file_to_be_signed, "redhat", write=True)
    assert signed.startswith("-----BEGIN PGP SIGNATURE-----\n")
    assert ks.verify_file(key, file_to_be_signed, file_to_be_signed + ".asc")
