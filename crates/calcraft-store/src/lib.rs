//! Storage layer of CalCraft.
//!
//! SQLite is the default backend. All queries are parameterised; no SQL is
//! ever built from user input. A PostgreSQL backend and a storage trait
//! follow once a second backend exists.

use std::{fmt, path::Path, str::FromStr, time::Duration};

use calcraft_core::{CollectionKind, Slug, Username};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Slug of the calendar and address book created for every new user.
pub const DEFAULT_COLLECTION_SLUG: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("database migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("user {0} already exists")]
    UserExists(Username),
    #[error("corrupt data in database: {0}")]
    Corrupt(String),
    #[error("failed to set permissions on database file: {0}")]
    Permissions(std::io::Error),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// A user account without credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub username: Username,
    pub display_name: Option<String>,
    pub disabled: bool,
}

/// A user together with the password hash, for authentication only.
#[derive(Clone)]
pub struct UserCredentials {
    pub user: User,
    pub password_hash: String,
}

impl fmt::Debug for UserCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserCredentials")
            .field("user", &self.user)
            .field("password_hash", &"<redacted>")
            .finish()
    }
}

/// A calendar or address book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: i64,
    pub owner_id: i64,
    pub kind: CollectionKind,
    pub slug: Slug,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub sync_revision: i64,
}

/// Handle to the database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Opens (and creates, if missing) the database file and applies migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        // SQLite keeps recent pages (including password hashes) in the
        // -wal and -shm files and reuses existing files with their
        // permissions, so all three are created owner-only up front.
        for file in database_files(path) {
            create_private(&file)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        let store = Self::init(pool).await?;
        for file in database_files(path) {
            restrict_permissions(&file)?;
        }
        Ok(store)
    }

    /// Opens a private in-memory database, for tests.
    pub async fn in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        // A single connection, because every connection gets its own in-memory database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Self::init(pool).await
    }

    async fn init(pool: SqlitePool) -> Result<Self> {
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    /// Checks that the database is reachable.
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Creates a user with a default calendar and address book.
    pub async fn create_user(
        &self,
        username: &Username,
        password_hash: &str,
        display_name: Option<&str>,
    ) -> Result<User> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "INSERT INTO users (username, password_hash, display_name) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(username.as_str())
        .bind(password_hash)
        .bind(display_name)
        .fetch_one(&mut *tx)
        .await;
        let id: i64 = match result {
            Ok(row) => row.try_get("id")?,
            Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
                return Err(StoreError::UserExists(username.clone()));
            }
            Err(err) => return Err(err.into()),
        };
        for (kind, name) in [
            (CollectionKind::Calendar, "Calendar"),
            (CollectionKind::Addressbook, "Contacts"),
        ] {
            sqlx::query(
                "INSERT INTO collections (owner_id, kind, slug, display_name) VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(kind.as_str())
            .bind(DEFAULT_COLLECTION_SLUG)
            .bind(name)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(User {
            id,
            username: username.clone(),
            display_name: display_name.map(str::to_owned),
            disabled: false,
        })
    }

    /// Looks up a user including the password hash.
    pub async fn user_credentials(&self, username: &Username) -> Result<Option<UserCredentials>> {
        let row = sqlx::query(
            "SELECT id, username, display_name, disabled, password_hash FROM users WHERE username = ?",
        )
        .bind(username.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(UserCredentials {
                user: user_from_row(&row)?,
                password_hash: row.try_get("password_hash")?,
            })
        })
        .transpose()
    }

    pub async fn list_users(&self) -> Result<Vec<User>> {
        let rows =
            sqlx::query("SELECT id, username, display_name, disabled FROM users ORDER BY username")
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(user_from_row).collect()
    }

    /// Replaces the password hash. Returns `false` if the user does not exist.
    pub async fn set_password_hash(
        &self,
        username: &Username,
        password_hash: &str,
    ) -> Result<bool> {
        let result = sqlx::query("UPDATE users SET password_hash = ? WHERE username = ?")
            .bind(password_hash)
            .bind(username.as_str())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Enables or disables a user. Returns `false` if the user does not exist.
    pub async fn set_disabled(&self, username: &Username, disabled: bool) -> Result<bool> {
        let result = sqlx::query("UPDATE users SET disabled = ? WHERE username = ?")
            .bind(disabled)
            .bind(username.as_str())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// All collections of one kind owned by a user.
    pub async fn collections(
        &self,
        owner_id: i64,
        kind: CollectionKind,
    ) -> Result<Vec<Collection>> {
        let rows = sqlx::query(
            "SELECT id, owner_id, kind, slug, display_name, description, color, sync_revision \
             FROM collections WHERE owner_id = ? AND kind = ? ORDER BY slug",
        )
        .bind(owner_id)
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(collection_from_row).collect()
    }

    /// A single collection of a user.
    pub async fn collection(
        &self,
        owner_id: i64,
        kind: CollectionKind,
        slug: &Slug,
    ) -> Result<Option<Collection>> {
        let row = sqlx::query(
            "SELECT id, owner_id, kind, slug, display_name, description, color, sync_revision \
             FROM collections WHERE owner_id = ? AND kind = ? AND slug = ?",
        )
        .bind(owner_id)
        .bind(kind.as_str())
        .bind(slug.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(collection_from_row).transpose()
    }
}

fn user_from_row(row: &SqliteRow) -> Result<User> {
    let username: String = row.try_get("username")?;
    Ok(User {
        id: row.try_get("id")?,
        username: username
            .try_into()
            .map_err(|err| StoreError::Corrupt(format!("{err}")))?,
        display_name: row.try_get("display_name")?,
        disabled: row.try_get("disabled")?,
    })
}

fn collection_from_row(row: &SqliteRow) -> Result<Collection> {
    let kind: String = row.try_get("kind")?;
    let slug: String = row.try_get("slug")?;
    Ok(Collection {
        id: row.try_get("id")?,
        owner_id: row.try_get("owner_id")?,
        kind: kind
            .parse()
            .map_err(|()| StoreError::Corrupt(format!("unknown collection kind {kind:?}")))?,
        slug: slug
            .try_into()
            .map_err(|err| StoreError::Corrupt(format!("{err}")))?,
        display_name: row.try_get("display_name")?,
        description: row.try_get("description")?,
        color: row.try_get("color")?,
        sync_revision: row.try_get("sync_revision")?,
    })
}

/// The database file and its WAL companions.
fn database_files(path: &Path) -> [std::path::PathBuf; 3] {
    let with_suffix = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        std::path::PathBuf::from(name)
    };
    [path.to_path_buf(), with_suffix("-wal"), with_suffix("-shm")]
}

/// Creates an empty file with owner-only permissions if it does not exist.
#[cfg(unix)]
fn create_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(StoreError::Permissions(err)),
    }
}

/// On Windows the files inherit the ACL of their directory.
#[cfg(not(unix))]
fn create_private(_path: &Path) -> Result<()> {
    Ok(())
}

/// Makes a file readable by the owner only, if it exists.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(StoreError::Permissions(err)),
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> Username {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn create_user_with_default_collections() {
        let store = Store::in_memory().await.unwrap();
        let user = store
            .create_user(&name("alice"), "hash", Some("Alice"))
            .await
            .unwrap();

        let calendars = store
            .collections(user.id, CollectionKind::Calendar)
            .await
            .unwrap();
        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].slug.as_str(), DEFAULT_COLLECTION_SLUG);
        let books = store
            .collections(user.id, CollectionKind::Addressbook)
            .await
            .unwrap();
        assert_eq!(books.len(), 1);

        let creds = store
            .user_credentials(&name("alice"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(creds.user, user);
        assert_eq!(creds.password_hash, "hash");
        assert!(!format!("{creds:?}").contains("hash\""));
    }

    #[tokio::test]
    async fn duplicate_user_is_rejected() {
        let store = Store::in_memory().await.unwrap();
        store.create_user(&name("bob"), "h", None).await.unwrap();
        let err = store
            .create_user(&name("bob"), "h", None)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::UserExists(_)));
    }

    #[tokio::test]
    async fn collections_are_isolated_per_user() {
        let store = Store::in_memory().await.unwrap();
        let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
        let bob = store.create_user(&name("bob"), "h", None).await.unwrap();
        let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();

        let a = store
            .collection(alice.id, CollectionKind::Calendar, &slug)
            .await
            .unwrap()
            .unwrap();
        let b = store
            .collection(bob.id, CollectionKind::Calendar, &slug)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(a.owner_id, alice.id);
        assert_eq!(b.owner_id, bob.id);
    }

    #[tokio::test]
    async fn update_password_and_disable() {
        let store = Store::in_memory().await.unwrap();
        store
            .create_user(&name("carol"), "old", None)
            .await
            .unwrap();
        assert!(
            store
                .set_password_hash(&name("carol"), "new")
                .await
                .unwrap()
        );
        assert!(store.set_disabled(&name("carol"), true).await.unwrap());
        assert!(!store.set_disabled(&name("nobody"), true).await.unwrap());

        let creds = store
            .user_credentials(&name("carol"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(creds.password_hash, "new");
        assert!(creds.user.disabled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn database_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("calcraft-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let store = Store::open(&path).await.unwrap();
        store.ping().await.unwrap();
        store
            .create_user(&name("walcheck"), "secret-hash", None)
            .await
            .unwrap();
        for file in database_files(&path) {
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{}", file.display());
        }
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
