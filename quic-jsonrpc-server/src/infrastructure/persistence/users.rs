//! Local credentials and reserved, explicitly linked future OIDC identities.

use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use async_trait::async_trait;
use mongodb::{
    Client, Collection, Database, IndexModel,
    bson::{DateTime, Document, doc, oid::ObjectId},
    options::IndexOptions,
};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::{
    domain::{Authenticator, RpcError, RpcResult, normalize_login, normalize_md5},
    infrastructure::config::MongoOptions,
};

use super::generate_session_id;

const USERS_COLLECTION: &str = "users";
const PROFILE_COLLECTION: &str = "profile";

#[derive(Serialize, Deserialize)]
struct ProfileDocument {
    #[serde(rename = "_id")]
    id: ObjectId,
    user_id: ObjectId,
    name: String,
    created_at: DateTime,
    updated_at: DateTime,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalIdentity {
    pub issuer: String,
    pub subject: String,
}

#[derive(Serialize, Deserialize)]
struct UserDocument {
    #[serde(rename = "_id")]
    id: ObjectId,
    login_normalized: String,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    password_hash: Option<String>,
    identities: Vec<ExternalIdentity>,
    created_at: DateTime,
    updated_at: DateTime,
}

#[derive(Clone)]
pub struct MongoAuthenticator {
    collection: Collection<UserDocument>,
    profiles: Collection<ProfileDocument>,
    dummy_hash: String,
    hash_slots: Arc<Semaphore>,
    session_ttl: Duration,
}

async fn prepare_profiles(database: &Database, name: &str) -> Result<Collection<ProfileDocument>> {
    let validator = doc! {"$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "user_id", "name", "created_at", "updated_at"],
        "additionalProperties": false,
        "properties": {
            "_id": {"bsonType": "objectId"},
            "user_id": {"bsonType": "objectId"},
            "name": {"bsonType": "string", "minLength": 2, "maxLength": 100},
            "created_at": {"bsonType": "date"},
            "updated_at": {"bsonType": "date"}
        }
    }};
    match database.create_collection(name).validator(validator.clone()).await {
        Ok(_) => {}
        Err(error) if has_code(&error, 48) => {}
        Err(_) => return Err(anyhow!("Unable to create profile collection")),
    }
    database
        .run_command(doc! {
            "collMod": name, "validator": validator,
            "validationLevel": "strict", "validationAction": "error"
        })
        .await
        .map_err(|_| anyhow!("Unable to configure profile validator"))?;
    let collection = database.collection::<ProfileDocument>(name);
    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! {"user_id": 1})
                .options(
                    IndexOptions::builder()
                        .name("profile_user_unique".to_string())
                        .unique(true)
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|_| anyhow!("Unable to create unique profile user index"))?;
    Ok(collection)
}

fn user_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "login_normalized", "enabled", "identities", "created_at", "updated_at"],
            "additionalProperties": false,
            "properties": {
                "_id": {"bsonType": "objectId"},
                "login_normalized": {
                    "bsonType": "string", "maxLength": 254,
                    "pattern": "^(?=[^@]{1,64}@)[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*@[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*$"
                },
                "enabled": {"bsonType": "bool"},
                "password_hash": {
                    "bsonType": "string", "maxLength": 256,
                    "pattern": "^\\$argon2id\\$v=19\\$"
                },
                "identities": {
                    "bsonType": "array", "uniqueItems": true,
                    "items": {
                        "bsonType": "object",
                        "required": ["issuer", "subject"],
                        "additionalProperties": false,
                        "properties": {
                            "issuer": {"bsonType": "string", "minLength": 1, "maxLength": 2048},
                            "subject": {"bsonType": "string", "minLength": 1, "maxLength": 255}
                        }
                    }
                },
                "created_at": {"bsonType": "date"},
                "updated_at": {"bsonType": "date"}
            }
        }
    }
}

fn has_code(error: &mongodb::error::Error, code: i32) -> bool {
    use mongodb::error::{ErrorKind, WriteFailure};
    match &*error.kind {
        ErrorKind::Command(error) => error.code == code,
        ErrorKind::Write(WriteFailure::WriteError(error)) => error.code == code,
        _ => false,
    }
}

async fn prepare_collection(database: &Database, collection_name: &str) -> Result<Collection<UserDocument>> {
    // Code 48 is NamespaceExists, including when another server wins the startup race.
    match database
        .create_collection(collection_name)
        .validator(user_validator())
        .await
    {
        Ok(_) => {}
        Err(error) if has_code(&error, 48) => {}
        Err(_) => return Err(anyhow!("Unable to create users collection")),
    }
    database
        .run_command(doc! {
            "collMod": collection_name,
            "validator": user_validator(),
            "validationLevel": "strict",
            "validationAction": "error",
        })
        .await
        .map_err(|_| anyhow!("Unable to configure users validator"))?;
    let collection = database.collection::<UserDocument>(collection_name);
    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! {"login_normalized": 1})
                .options(
                    IndexOptions::builder()
                        .name("users_login_unique".to_string())
                        .unique(true)
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|_| anyhow!("Unable to create unique users login index"))?;
    // Both fields are from the same array element. Missing/empty identity arrays are excluded;
    // a provider label or matching email is never an external identity.
    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! {"identities.issuer": 1, "identities.subject": 1})
                .options(
                    IndexOptions::builder()
                        .name("users_oidc_identity_unique".to_string())
                        .unique(true)
                        .partial_filter_expression(doc! {
                            "identities.issuer": {"$type": "string"},
                            "identities.subject": {"$type": "string"}
                        })
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|_| anyhow!("Unable to create unique external identity index"))?;
    Ok(collection)
}

fn hash_password_equivalent(password: &str) -> Result<String> {
    let mut salt = [0u8; 16];
    rand::rngs::OsRng
        .try_fill_bytes(&mut salt)
        .map_err(|_| anyhow!("Secure randomness unavailable"))?;
    let salt = SaltString::encode_b64(&salt).map_err(|_| anyhow!("Unable to generate password salt"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| anyhow!("Unable to hash password equivalent"))
}

fn verify_password_equivalent(password: &str, encoded: &str) -> RpcResult<bool> {
    let parsed = PasswordHash::new(encoded).map_err(|_| RpcError::authentication_unavailable())?;
    if parsed.algorithm.as_str() != "argon2id" {
        return Err(RpcError::authentication_unavailable());
    }
    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(_) => Err(RpcError::authentication_unavailable()),
    }
}

impl MongoAuthenticator {
    /// Creates the empty validated collection/indexes; never creates accounts.
    pub async fn connect(options: &MongoOptions, session_ttl: Duration) -> Result<Self> {
        Self::connect_collection(options, session_ttl, USERS_COLLECTION).await
    }

    async fn connect_collection(options: &MongoOptions, session_ttl: Duration, collection_name: &str) -> Result<Self> {
        let client = Client::with_uri_str(&options.uri)
            .await
            .map_err(|_| anyhow!("Unable to initialize authentication database"))?;
        let collection = prepare_collection(&client.database(&options.database), collection_name).await?;
        let profile_name = if collection_name == USERS_COLLECTION {
            PROFILE_COLLECTION.to_string()
        } else {
            format!("{collection_name}_profile")
        };
        let profiles = prepare_profiles(&client.database(&options.database), &profile_name).await?;
        let dummy_hash = tokio::task::spawn_blocking(|| hash_password_equivalent(&generate_session_id()))
            .await
            .map_err(|_| anyhow!("Password hashing worker failed"))??;
        Ok(Self {
            collection,
            profiles,
            dummy_hash,
            hash_slots: Arc::new(Semaphore::new(4)),
            session_ttl,
        })
    }

    /// Operator-only provisioning, with the same normalization/hash/schema as login.
    /// The MD5 is a password-equivalent secret and must not appear in arguments or logs.
    pub async fn create_user(&self, login: &str, md5: &str) -> Result<ObjectId> {
        let login_normalized = normalize_login(login).map_err(|_| anyhow!("Invalid email login"))?;
        let md5 = normalize_md5(md5).map_err(|_| anyhow!("Expected exactly 32 hexadecimal MD5 characters"))?;
        let permit = self
            .hash_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("Password hashing unavailable"))?;
        let password_hash = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hash_password_equivalent(&md5)
        })
        .await
        .map_err(|_| anyhow!("Password hashing worker failed"))??;
        let id = ObjectId::new();
        let now = DateTime::now();
        let user = UserDocument {
            id,
            login_normalized,
            enabled: true,
            password_hash: Some(password_hash),
            identities: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        match self.collection.insert_one(user).await {
            Ok(_) => Ok(id),
            Err(error) if has_code(&error, 11000) => Err(anyhow!("User login already exists")),
            Err(_) => Err(anyhow!("Unable to create user")),
        }
    }

    async fn cleanup_account(&self, user_id: ObjectId, profile_id: ObjectId) -> RpcResult<()> {
        // Delete the owned user first: never remove its profile while an enabled user might remain.
        self.collection
            .delete_one(doc! {"_id": user_id})
            .await
            .map_err(|_| RpcError::account_cleanup_incomplete())?;
        self.profiles
            .delete_one(doc! {"_id": profile_id, "user_id": user_id})
            .await
            .map_err(|_| RpcError::account_cleanup_incomplete())?;
        Ok(())
    }

    async fn provision_account(&self, command: crate::domain::authenticator::CreateAccountCommand) -> RpcResult<Value> {
        let permit = self
            .hash_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RpcError::account_creation_unavailable())?;
        let password_hash = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hash_password_equivalent(&command.password)
        })
        .await
        .map_err(|_| RpcError::account_creation_unavailable())?
        .map_err(|_| RpcError::account_creation_unavailable())?;
        let user_id = ObjectId::new();
        let profile_id = ObjectId::new();
        let now = DateTime::now();
        let user = UserDocument {
            id: user_id,
            login_normalized: command.email,
            enabled: false,
            password_hash: Some(password_hash),
            identities: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        if let Err(error) = self.collection.insert_one(user).await {
            if has_code(&error, 11000) {
                return Err(RpcError::account_exists());
            }
            // A network error may mean that an insert committed but its acknowledgement was lost.
            self.cleanup_account(user_id, profile_id).await?;
            return Err(RpcError::account_creation_unavailable());
        }
        let result = async {
            self.profiles
                .insert_one(ProfileDocument {
                    id: profile_id,
                    user_id,
                    name: command.name,
                    created_at: now,
                    updated_at: now,
                })
                .await
                .map_err(|_| RpcError::account_creation_unavailable())?;
            let enabled = self
                .collection
                .update_one(
                    doc! {"_id": user_id, "enabled": false},
                    doc! {"$set": {"enabled": true, "updated_at": DateTime::now()}},
                )
                .await
                .map_err(|_| RpcError::account_creation_unavailable())?;
            if enabled.modified_count != 1 {
                return Err(RpcError::account_creation_unavailable());
            }
            Ok(json!({"status": "ok", "created": true}))
        }
        .await;
        if result.is_err() {
            self.cleanup_account(user_id, profile_id).await?;
        }
        result
    }
}

#[async_trait]
impl Authenticator for MongoAuthenticator {
    async fn create_account(&self, command: crate::domain::authenticator::CreateAccountCommand) -> RpcResult<Value> {
        let command =
            crate::domain::authenticator::CreateAccountCommand::new(&command.name, &command.email, &command.password)?;
        // A disconnected/cancelled caller must not cancel provisioning between the two writes.
        let owned = self.clone();
        tokio::spawn(async move { owned.provision_account(command).await })
            .await
            .map_err(|_| RpcError::account_cleanup_incomplete())?
    }

    async fn authenticate(&self, payload: Value) -> RpcResult<Value> {
        let login = normalize_login(
            payload
                .get("login")
                .and_then(Value::as_str)
                .ok_or_else(RpcError::invalid_params)?,
        )?;
        let password = normalize_md5(
            payload
                .get("password")
                .and_then(Value::as_str)
                .ok_or_else(RpcError::invalid_params)?,
        )?;
        let user = self
            .collection
            .find_one(doc! {"login_normalized": &login})
            .await
            .map_err(|_| RpcError::authentication_unavailable())?;
        let eligible = user
            .as_ref()
            .is_some_and(|user| user.enabled && user.password_hash.is_some());
        let hash = user
            .as_ref()
            .filter(|_| eligible)
            .and_then(|user| user.password_hash.clone())
            .unwrap_or_else(|| self.dummy_hash.clone());
        // Unknown, disabled and external-only users pay the same verification cost.
        let permit = self
            .hash_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RpcError::authentication_unavailable())?;
        let valid = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            verify_password_equivalent(&password, &hash)
        })
        .await
        .map_err(|_| RpcError::authentication_unavailable())??;
        if !eligible || !valid {
            return Err(RpcError::invalid_credentials());
        }
        let user = user.ok_or_else(RpcError::invalid_credentials)?;
        Ok(json!({
            "status": "ok",
            "user": {"id": user.id.to_hex(), "login": login},
            "session": {
                "tokenType": "Bearer",
                "accessToken": format!("local-{}", generate_session_id()),
                "expiresInSec": self.session_ttl.as_secs()
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_salted_argon2id_and_verify_only_the_password_equivalent() {
        let password = "5f4dcc3b5aa765d61d8327deb882cf99";
        let first = hash_password_equivalent(password).unwrap();
        let second = hash_password_equivalent(password).unwrap();
        assert!(first.starts_with("$argon2id$v=19$"));
        assert!(!first.contains(password));
        assert_ne!(first, second);
        assert!(verify_password_equivalent(password, &first).unwrap());
        assert!(!verify_password_equivalent("00000000000000000000000000000000", &first).unwrap());
        assert!(verify_password_equivalent(password, "invalid hash").is_err());
    }

    #[tokio::test]
    #[ignore = "requires configured MongoDB; creates and drops an isolated test collection"]
    async fn mongo_users_authentication_and_schema() {
        // Read dotenv without mutating the multithreaded test process's environment.
        let file = crate::env::crate_dir().join(".env");
        let values = if file.is_file() {
            dotenvy::from_path_iter(file)
                .expect("read test configuration")
                .collect::<std::result::Result<std::collections::HashMap<_, _>, _>>()
                .map_err(|_| "Invalid test configuration")
                .unwrap()
        } else {
            std::collections::HashMap::new()
        };
        let options = MongoOptions::from_lookup(|key| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| values.get(key).filter(|value| !value.trim().is_empty()).cloned())
        })
        .expect("Mongo test configuration");
        let collection_name = format!("users_auth_test_{}", ObjectId::new().to_hex());
        let client = Client::with_uri_str(&options.uri).await.unwrap();
        let database = client.database(&options.database);
        let auth = MongoAuthenticator::connect_collection(&options, Duration::from_secs(60), &collection_name)
            .await
            .unwrap();
        let payload = json!({"login": "driver@example.com", "password": "5f4dcc3b5aa765d61d8327deb882cf99"});
        assert_eq!(
            auth.authenticate(payload.clone()).await.unwrap_err(),
            RpcError::invalid_credentials()
        );
        let id = auth
            .create_user(" DRIVER@Example.com ", payload["password"].as_str().unwrap())
            .await
            .unwrap();
        assert!(
            auth.create_user("driver@example.com", payload["password"].as_str().unwrap())
                .await
                .is_err()
        );
        let mut normalized = payload.clone();
        normalized["login"] = json!(" DRIVER@EXAMPLE.COM ");
        normalized["password"] = json!("5F4DCC3B5AA765D61D8327DEB882CF99");
        assert_eq!(auth.authenticate(normalized).await.unwrap()["status"], "ok");
        let mut wrong = payload.clone();
        wrong["password"] = json!("00000000000000000000000000000000");
        assert_eq!(
            auth.authenticate(wrong).await.unwrap_err(),
            RpcError::invalid_credentials()
        );
        auth.collection
            .update_one(doc! {"_id": id}, doc! {"$set": {"enabled": false}})
            .await
            .unwrap();
        assert_eq!(
            auth.authenticate(payload.clone()).await.unwrap_err(),
            RpcError::invalid_credentials()
        );
        let raw = database.collection::<Document>(&collection_name);
        assert!(
            raw.update_one(
                doc! {"_id": id},
                doc! {"$set": {"password_hash": payload["password"].as_str().unwrap()}}
            )
            .await
            .is_err()
        );
        assert!(
            raw.update_one(
                doc! {"_id": id},
                doc! {"$set": {"login_normalized": "DRIVER@example.com"}}
            )
            .await
            .is_err()
        );
        raw.update_one(
            doc! {"_id": id},
            doc! {
                "$set": {"identities": [{"issuer": "https://issuer.example", "subject": "subject-1"}]}
            },
        )
        .await
        .unwrap();
        let second = auth
            .create_user("other@example.com", payload["password"].as_str().unwrap())
            .await
            .unwrap();
        assert!(
            raw.update_one(
                doc! {"_id": second},
                doc! {
                    "$set": {"identities": [{"issuer": "https://issuer.example", "subject": "subject-1"}]}
                }
            )
            .await
            .is_err()
        );
        raw.update_one(
            doc! {"_id": second},
            doc! {
                "$set": {"identities": [{"issuer": "https://other-issuer.example", "subject": "subject-1"}]}
            },
        )
        .await
        .unwrap();
        assert!(
            raw.update_one(
                doc! {"_id": second},
                doc! {
                    "$set": {"identities": [
                        {"issuer": "https://other-issuer.example", "subject": "subject-1"},
                        {"issuer": "https://other-issuer.example", "subject": "subject-1"}
                    ]}
                }
            )
            .await
            .is_err()
        );
        assert!(
            raw.update_one(
                doc! {"_id": second},
                doc! {
                    "$set": {"identities": [
                        {"issuer": "https://other-issuer.example", "subject": "subject-1"},
                        {"subject": "subject-1", "issuer": "https://other-issuer.example"}
                    ]}
                }
            )
            .await
            .is_err()
        );
        raw.update_one(doc! {"_id": second}, doc! {"$unset": {"password_hash": ""}})
            .await
            .unwrap();
        let external_only = json!({"login": "other@example.com", "password": payload["password"]});
        assert_eq!(
            auth.authenticate(external_only).await.unwrap_err(),
            RpcError::invalid_credentials()
        );
        // Re-running startup must preserve accounts and enforce the same indexes.
        MongoAuthenticator::connect_collection(&options, Duration::from_secs(60), &collection_name)
            .await
            .unwrap();
        assert_eq!(raw.count_documents(doc! {}).await.unwrap(), 2);
        raw.drop().await.unwrap();
        auth.profiles.drop().await.unwrap();
        assert_eq!(
            auth.authenticate(payload).await.unwrap_err(),
            RpcError::invalid_credentials()
        );
    }

    #[tokio::test]
    #[ignore = "requires local standalone MongoDB on 27017; creates and drops an isolated database"]
    async fn mongo_create_account_staged_provisioning() {
        // Never read production credentials or mutate production collections.
        let options = MongoOptions {
            uri: "mongodb://127.0.0.1:27017/?serverSelectionTimeoutMS=1000".to_string(),
            database: format!("brnavi_registration_test_{}", ObjectId::new().to_hex()),
        };
        let client = Client::with_uri_str(&options.uri).await.unwrap();
        let database = client.database(&options.database);
        let cleanup = database.clone();
        let outcome = tokio::spawn(async move {
            let auth = MongoAuthenticator::connect(&options, Duration::from_secs(60))
                .await
                .unwrap();
            let command = |email: &str| {
                crate::domain::authenticator::CreateAccountCommand::new(
                    "  Test Driver  ",
                    email,
                    "5f4dcc3b5aa765d61d8327deb882cf99",
                )
                .unwrap()
            };
            let users = database.collection::<Document>(USERS_COLLECTION);
            let profiles = database.collection::<Document>(PROFILE_COLLECTION);
            let sessions = Arc::new(crate::infrastructure::persistence::InMemorySessionRepository::new(
                Duration::from_secs(60),
            ));
            let service = Arc::new(crate::application::AuthService::with_authenticator(
                sessions.clone(),
                Arc::new(auth.clone()),
                Duration::from_secs(60),
            ));
            let dispatcher = crate::tests::dispatcher_with_auth(service);
            let context = crate::presentation::controller::StreamContext {
                connection_id: 1,
                stream_id: 1,
                remote_ip: "127.0.0.1".parse().unwrap(),
            };
            let registration = dispatcher
                .handle_request(
                    json!({
                        "jsonrpc": "2.0", "method": "createAccount", "id": 1,
                        "params": {"name": "  Test Driver  ", "email": " TEST@Example.com ",
                            "password": "5f4dcc3b5aa765d61d8327deb882cf99"}
                    }),
                    context,
                )
                .await
                .unwrap();
            assert_eq!(registration["result"], json!({"status": "ok", "created": true}));
            assert!(registration["result"].get("session").is_none());
            let original_user = users
                .find_one(doc! {"login_normalized": "test@example.com"})
                .await
                .unwrap()
                .unwrap();
            let user_id = original_user.get_object_id("_id").unwrap();
            assert!(original_user.get_bool("enabled").unwrap());
            assert!(!original_user.contains_key("name"));
            assert!(
                original_user
                    .get_str("password_hash")
                    .unwrap()
                    .starts_with("$argon2id$")
            );
            let original_profile = profiles.find_one(doc! {"user_id": user_id}).await.unwrap().unwrap();
            assert_eq!(original_profile.get_str("name").unwrap(), "Test Driver");
            let login = dispatcher
                .handle_request(
                    json!({
                        "jsonrpc": "2.0", "method": "login", "id": 2,
                        "params": {"login": " TEST@Example.com ", "password": "5f4dcc3b5aa765d61d8327deb882cf99"}
                    }),
                    context,
                )
                .await
                .unwrap();
            assert_eq!(login["result"]["status"], "ok");
            let session_id = login["result"]["session"]["sessionId"].as_str().unwrap();
            use crate::domain::SessionRepository;
            assert!(sessions.find(session_id).await.unwrap().is_some());
            assert_eq!(
                auth.authenticate(json!({
                    "login": "test@example.com", "password": "5f4dcc3b5aa765d61d8327deb882cf99"
                }))
                .await
                .unwrap()["status"],
                "ok"
            );
            assert_eq!(
                auth.create_account(command("Test@example.com")).await.unwrap_err(),
                RpcError::account_exists()
            );
            assert_eq!(
                users.find_one(doc! {"_id": user_id}).await.unwrap().unwrap(),
                original_user
            );
            assert_eq!(
                profiles.find_one(doc! {"user_id": user_id}).await.unwrap().unwrap(),
                original_profile
            );
            // Preserve operator provisioning: an enabled CLI user need not have a profile.
            let admin_id = auth
                .create_user("operator@example.com", "5f4dcc3b5aa765d61d8327deb882cf99")
                .await
                .unwrap();
            assert!(
                users
                    .find_one(doc! {"_id": admin_id})
                    .await
                    .unwrap()
                    .unwrap()
                    .get_bool("enabled")
                    .unwrap()
            );
            assert!(profiles.find_one(doc! {"user_id": admin_id}).await.unwrap().is_none());
            assert_eq!(
                auth.authenticate(json!({
                    "login": " OPERATOR@EXAMPLE.COM ", "password": "5F4DCC3B5AA765D61D8327DEB882CF99"
                }))
                .await
                .unwrap()["status"],
                "ok"
            );
            assert!(
                auth.create_user("operator@example.com", "5f4dcc3b5aa765d61d8327deb882cf99")
                    .await
                    .is_err()
            );
            users
                .update_one(doc! {"_id": admin_id}, doc! {"$set": {"enabled": false}})
                .await
                .unwrap();
            assert_eq!(
                auth.authenticate(json!({
                    "login": "operator@example.com", "password": "5f4dcc3b5aa765d61d8327deb882cf99"
                }))
                .await
                .unwrap_err(),
                RpcError::invalid_credentials()
            );
            users.delete_one(doc! {"_id": admin_id}).await.unwrap();
            assert!(
                profiles
                    .insert_one(doc! {
                        "_id": ObjectId::new(), "user_id": user_id, "name": "Other Driver",
                        "created_at": DateTime::now(), "updated_at": DateTime::now()
                    })
                    .await
                    .is_err()
            );
            assert!(
                profiles
                    .update_one(doc! {"user_id": user_id}, doc! {"$set": {"user_id": "not-object-id"}})
                    .await
                    .is_err()
            );

            let (first, second) = tokio::join!(
                auth.create_account(command("race@example.com")),
                auth.create_account(command(" RACE@EXAMPLE.COM "))
            );
            assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
            let duplicate = first.err().or_else(|| second.err()).unwrap();
            assert_eq!(duplicate, RpcError::account_exists());
            assert_eq!(users.count_documents(doc! {}).await.unwrap(), 2);
            assert_eq!(profiles.count_documents(doc! {}).await.unwrap(), 2);

            // A profile write failure compensates only this attempt's disabled user.
            database
                .run_command(doc! {"collMod": PROFILE_COLLECTION,
                    "validator": {"name": {"$eq": "reject-all-new-profiles"}}
                })
                .await
                .unwrap();
            assert_eq!(
                auth.create_account(command("profile-fail@example.com"))
                    .await
                    .unwrap_err(),
                RpcError::account_creation_unavailable()
            );
            assert_eq!(users.count_documents(doc! {}).await.unwrap(), 2);
            assert_eq!(profiles.count_documents(doc! {}).await.unwrap(), 2);
            assert_eq!(
                auth.authenticate(json!({
                    "login": "profile-fail@example.com", "password": "5f4dcc3b5aa765d61d8327deb882cf99"
                }))
                .await
                .unwrap_err(),
                RpcError::invalid_credentials()
            );
            prepare_profiles(&database, PROFILE_COLLECTION).await.unwrap();

            // Reject enabling after a successful profile insert; both new records must disappear.
            database
                .run_command(doc! {"collMod": USERS_COLLECTION,
                    "validator": {"$and": [user_validator(), {"enabled": false}]}
                })
                .await
                .unwrap();
            assert_eq!(
                auth.create_account(command("enable-fail@example.com"))
                    .await
                    .unwrap_err(),
                RpcError::account_creation_unavailable()
            );
            assert_eq!(users.count_documents(doc! {}).await.unwrap(), 2);
            assert_eq!(profiles.count_documents(doc! {}).await.unwrap(), 2);
            prepare_collection(&database, USERS_COLLECTION).await.unwrap();

            // An inaccessible profile adapter makes compensating cleanup explicitly fail.
            let unavailable = Client::with_uri_str("mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=50")
                .await
                .unwrap();
            let mut broken = auth.clone();
            broken.profiles = unavailable.database(&options.database).collection(PROFILE_COLLECTION);
            assert_eq!(
                broken
                    .create_account(command("cleanup-fail@example.com"))
                    .await
                    .unwrap_err(),
                RpcError::account_cleanup_incomplete()
            );
            assert_eq!(users.count_documents(doc! {}).await.unwrap(), 2);
            assert_eq!(profiles.count_documents(doc! {}).await.unwrap(), 2);
            assert_eq!(
                users.find_one(doc! {"_id": user_id}).await.unwrap().unwrap(),
                original_user
            );
            assert_eq!(
                profiles.find_one(doc! {"user_id": user_id}).await.unwrap().unwrap(),
                original_profile
            );
        })
        .await;
        cleanup
            .drop()
            .await
            .expect("remove isolated registration test database");
        outcome.expect("registration integration assertions");
    }

    #[tokio::test]
    async fn database_failure_is_sanitized_and_not_invalid_credentials() {
        let client = Client::with_uri_str("mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=50")
            .await
            .unwrap();
        let auth = MongoAuthenticator {
            collection: client.database("unavailable").collection(USERS_COLLECTION),
            profiles: client.database("unavailable").collection(PROFILE_COLLECTION),
            dummy_hash: String::new(),
            hash_slots: Arc::new(Semaphore::new(4)),
            session_ttl: Duration::from_secs(60),
        };
        let error = auth
            .authenticate(json!({
                "login": "driver@example.com",
                "password": "5f4dcc3b5aa765d61d8327deb882cf99"
            }))
            .await
            .unwrap_err();
        assert_eq!(error, RpcError::authentication_unavailable());
        assert!(!error.message.contains("mongodb"));
    }
}
