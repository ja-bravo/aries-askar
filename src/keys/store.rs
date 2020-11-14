use chacha20poly1305::{
    aead::{
        generic_array::typenum::{Unsigned, U32},
        Aead, NewAead,
    },
    ChaCha20Poly1305, Key as ChaChaKey, Nonce,
};
use hmac::{Hmac, Mac, NewMac};
use indy_utils::{keys::ArrayKey, random::random_array};
use sha2::Sha256;

use serde::Deserialize;

use crate::error::Result;
use crate::keys::EntryEncryptor;
use crate::types::{EncEntryTag, EntryTag};

const ENC_KEY_BYTES: usize = <ChaCha20Poly1305 as NewAead>::KeySize::USIZE;
const ENC_KEY_SIZE: usize = <ChaCha20Poly1305 as Aead>::NonceSize::USIZE
    + ENC_KEY_BYTES
    + <ChaCha20Poly1305 as Aead>::TagSize::USIZE;

pub type EncKey = ArrayKey<U32>;
pub type HmacKey = ArrayKey<U32>;
type NonceSize = <ChaCha20Poly1305 as Aead>::NonceSize;
type TagSize = <ChaCha20Poly1305 as Aead>::TagSize;

/// A store key combining the keys required to encrypt
/// and decrypt storage records
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct StoreKey {
    pub category_key: EncKey,
    pub name_key: EncKey,
    pub value_key: EncKey,
    pub item_hmac_key: HmacKey,
    pub tag_name_key: EncKey,
    pub tag_value_key: EncKey,
    pub tags_hmac_key: HmacKey,
}

impl StoreKey {
    pub fn new() -> Result<Self> {
        Ok(Self {
            category_key: ArrayKey::random(),
            name_key: ArrayKey::random(),
            value_key: ArrayKey::random(),
            item_hmac_key: ArrayKey::random(),
            tag_name_key: ArrayKey::random(),
            tag_value_key: ArrayKey::random(),
            tags_hmac_key: ArrayKey::random(),
        })
    }

    pub fn encrypt_category<B: AsRef<[u8]>>(&self, category: B) -> Result<Vec<u8>> {
        encrypt_searchable(&self.category_key, &self.item_hmac_key, category.as_ref())
    }

    pub fn encrypt_name<B: AsRef<[u8]>>(&self, name: B) -> Result<Vec<u8>> {
        encrypt_searchable(&self.name_key, &self.item_hmac_key, name.as_ref())
    }

    pub fn encrypt_value<B: AsRef<[u8]>>(&self, value: B) -> Result<Vec<u8>> {
        let value_key = ArrayKey::random();
        let mut value = encrypt_non_searchable(&value_key, value.as_ref())?;
        let mut result = encrypt_non_searchable(&self.value_key, value_key.as_ref())?;
        result.append(&mut value);
        Ok(result)
    }

    pub fn encrypt_tag_name<B: AsRef<[u8]>>(&self, name: B) -> Result<Vec<u8>> {
        encrypt_searchable(&self.tag_name_key, &self.tags_hmac_key, name.as_ref())
    }

    pub fn encrypt_tag_value<B: AsRef<[u8]>>(&self, value: B) -> Result<Vec<u8>> {
        encrypt_searchable(&self.tag_value_key, &self.tags_hmac_key, value.as_ref())
    }

    pub fn decrypt_category<B: AsRef<[u8]>>(&self, enc_category: B) -> Result<Vec<u8>> {
        decrypt(&self.category_key, enc_category.as_ref())
    }

    pub fn decrypt_name<B: AsRef<[u8]>>(&self, enc_name: B) -> Result<Vec<u8>> {
        decrypt(&self.name_key, enc_name.as_ref())
    }

    pub fn decrypt_value<B: AsRef<[u8]>>(&self, enc_value: B) -> Result<Vec<u8>> {
        let enc_value = enc_value.as_ref();
        if enc_value.len() < ENC_KEY_SIZE + TagSize::USIZE {
            return Err(err_msg!(
                Encryption,
                "Buffer is too short to represent an encrypted value",
            ));
        }
        let value = &enc_value[ENC_KEY_SIZE..];
        let value_key = ArrayKey::from_slice(decrypt(&self.value_key, &enc_value[..ENC_KEY_SIZE])?);
        decrypt(&value_key, value)
    }

    pub fn decrypt_tag_name<B: AsRef<[u8]>>(&self, enc_tag_name: B) -> Result<Vec<u8>> {
        decrypt(&self.tag_name_key, enc_tag_name.as_ref())
    }

    pub fn decrypt_tag_value<B: AsRef<[u8]>>(&self, enc_tag_value: B) -> Result<Vec<u8>> {
        decrypt(&self.tag_value_key, enc_tag_value.as_ref())
    }

    pub fn to_string(&self) -> Result<String> {
        serde_json::to_string(self).map_err(err_map!(Unexpected, "Error serializing store key"))
    }

    pub fn from_slice(input: &[u8]) -> Result<Self> {
        serde_json::from_slice(input).map_err(err_map!(Unsupported, "Invalid store key"))
    }
}

/// Encrypt a value with a predictable nonce, making it searchable
pub fn encrypt_searchable(enc_key: &EncKey, hmac_key: &HmacKey, input: &[u8]) -> Result<Vec<u8>> {
    let chacha = ChaCha20Poly1305::new(ChaChaKey::from_slice(enc_key));
    let mut nonce_hmac =
        Hmac::<Sha256>::new_varkey(&**hmac_key).map_err(|e| err_msg!(Encryption, "{}", e))?;
    nonce_hmac.update(input);
    let result = nonce_hmac.finalize().into_bytes();
    let nonce = Nonce::from_slice(&result[0..NonceSize::USIZE]);
    let mut enc = chacha
        .encrypt(nonce, input)
        .map_err(|e| err_msg!(Encryption, "{}", e))?;
    let mut result = nonce.to_vec();
    result.append(&mut enc);
    Ok(result)
}

/// Encrypt a value with a random nonce
pub fn encrypt_non_searchable(enc_key: &EncKey, input: &[u8]) -> Result<Vec<u8>> {
    let chacha = ChaCha20Poly1305::new(ChaChaKey::from_slice(enc_key));
    let nonce = random_array();
    let mut enc = chacha
        .encrypt(&nonce, input)
        .map_err(|e| err_msg!(Encryption, "{}", e))?;
    let mut result = nonce.to_vec();
    result.append(&mut enc);
    Ok(result)
}

/// Decrypt a previously encrypted value with nonce attached
pub fn decrypt(enc_key: &EncKey, input: &[u8]) -> Result<Vec<u8>> {
    if input.len() < NonceSize::USIZE + TagSize::USIZE {
        return Err(err_msg!(Encryption, "Invalid length for encrypted buffer"));
    }
    let nonce = Nonce::from_slice(&input[0..NonceSize::USIZE]);
    let chacha = ChaCha20Poly1305::new(ChaChaKey::from_slice(enc_key));
    chacha
        .decrypt(&nonce, &input[NonceSize::USIZE..])
        .map_err(|e| err_msg!(Encryption, "Error decrypting record: {}", e))
}

impl EntryEncryptor for StoreKey {
    fn encrypt_entry_category(&self, category: &str) -> Result<Vec<u8>> {
        Ok(self.encrypt_category(&category)?)
    }

    fn encrypt_entry_name(&self, name: &str) -> Result<Vec<u8>> {
        Ok(self.encrypt_name(&name)?)
    }

    fn encrypt_entry_value(&self, value: &[u8]) -> Result<Vec<u8>> {
        Ok(self.encrypt_value(&value)?)
    }

    fn encrypt_entry_tags(&self, tags: &[EntryTag]) -> Result<Vec<EncEntryTag>> {
        tags.into_iter()
            .map(|tag| match tag {
                EntryTag::Plaintext(name, value) => {
                    let name = self.encrypt_tag_name(&name)?;
                    Ok(EncEntryTag {
                        name,
                        value: value.as_bytes().to_vec(),
                        plaintext: true,
                    })
                }
                EntryTag::Encrypted(name, value) => {
                    let name = self.encrypt_tag_name(&name)?;
                    let value = self.encrypt_tag_value(&value)?;
                    Ok(EncEntryTag {
                        name,
                        value,
                        plaintext: false,
                    })
                }
            })
            .collect()
    }

    fn decrypt_entry_category(&self, enc_category: &[u8]) -> Result<String> {
        decode_utf8(self.decrypt_category(&enc_category)?)
    }

    fn decrypt_entry_name(&self, enc_name: &[u8]) -> Result<String> {
        decode_utf8(self.decrypt_name(&enc_name)?)
    }

    fn decrypt_entry_value(&self, enc_value: &[u8]) -> Result<Vec<u8>> {
        Ok(self.decrypt_value(&enc_value)?)
    }

    fn decrypt_entry_tags(&self, enc_tags: &[EncEntryTag]) -> Result<Vec<EntryTag>> {
        enc_tags.into_iter().try_fold(vec![], |mut acc, tag| {
            let name = decode_utf8(self.decrypt_tag_name(&tag.name)?)?;
            acc.push(if tag.plaintext {
                let value = decode_utf8(tag.value.clone())?;
                EntryTag::Plaintext(name, value)
            } else {
                let value = decode_utf8(self.decrypt_tag_value(&tag.value)?)?;
                EntryTag::Encrypted(name, value)
            });
            Result::Ok(acc)
        })
    }
}

#[inline]
fn decode_utf8(value: Vec<u8>) -> Result<String> {
    String::from_utf8(value).map_err(err_map!(Encryption))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Entry;

    #[test]
    fn store_key_round_trip() {
        let key = StoreKey::new().unwrap();
        let test_record = Entry {
            category: "category".to_string(),
            name: "name".to_string(),
            value: b"value".to_vec(),
            tags: Some(vec![
                EntryTag::Plaintext("plain".to_string(), "tag".to_string()),
                EntryTag::Encrypted("enctag".to_string(), "envtagval".to_string()),
            ]),
        };
        let enc_category = key.encrypt_entry_category(&test_record.category).unwrap();
        let enc_name = key.encrypt_entry_name(&test_record.name).unwrap();
        let enc_value = key.encrypt_entry_value(&test_record.value).unwrap();
        let enc_tags = key
            .encrypt_entry_tags(&test_record.tags.as_ref().unwrap())
            .unwrap();
        assert_ne!(enc_category.as_slice(), test_record.category.as_bytes());
        assert_ne!(enc_name.as_slice(), test_record.name.as_bytes());
        assert_ne!(enc_value.as_slice(), test_record.value.as_slice());

        let cmp_record = Entry {
            category: key.decrypt_entry_category(&enc_category).unwrap(),
            name: key.decrypt_entry_name(&enc_name).unwrap(),
            value: key.decrypt_entry_value(&enc_value).unwrap(),
            tags: Some(key.decrypt_entry_tags(&enc_tags).unwrap()),
        };
        assert_eq!(test_record, cmp_record);
    }

    #[test]
    fn store_key_non_searchable() {
        let input = b"hello";
        let key = ArrayKey::random();
        let enc = encrypt_non_searchable(&key, input).unwrap();
        assert_eq!(enc.len(), input.len() + NonceSize::USIZE + TagSize::USIZE);
        let dec = decrypt(&key, enc.as_slice()).unwrap();
        assert_eq!(dec.as_slice(), input);
    }

    #[test]
    fn store_key_searchable() {
        let input = b"hello";
        let key = ArrayKey::random();
        let hmac_key = ArrayKey::random();
        let enc = encrypt_searchable(&key, &hmac_key, input).unwrap();
        assert_eq!(enc.len(), input.len() + NonceSize::USIZE + TagSize::USIZE);
        let dec = decrypt(&key, enc.as_slice()).unwrap();
        assert_eq!(dec.as_slice(), input);
    }

    #[test]
    fn store_key_serde() {
        let key = StoreKey::new().unwrap();
        let key_json = serde_json::to_string(&key).unwrap();
        let key_cmp = serde_json::from_str(&key_json).unwrap();
        assert_eq!(key, key_cmp);
    }
}
