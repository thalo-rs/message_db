use std::borrow::Cow;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde::Deserialize;
use serde_json::Value;
use sqlx::{Executor, PgPool, Postgres, Transaction};
use typed_builder::TypedBuilder;
use uuid::Uuid;

use crate::message::{DeserializeMessage, GenericMessage, Message, Metadata};
use crate::{Error, Result};

macro_rules! message_db_fn {
    ($s:literal) => {
        concat!(
            r#"
                SELECT
                id,
                stream_name,
                "type",
                "position",
                global_position,
                data::jsonb,
                metadata::jsonb,
                time
            FROM "#,
            $s
        )
    };
}

#[derive(Clone, Debug)]
pub struct MessageDb {
    pool: PgPool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, TypedBuilder)]
pub struct WriteMessageOpts {
    #[builder(default, setter(strip_option))]
    id: Option<String>,
    #[builder(default, setter(strip_option))]
    metadata: Option<Metadata>,
    #[builder(default, setter(strip_option))]
    expected_version: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, TypedBuilder)]
pub struct GetStreamMessagesOpts<'a> {
    #[builder(default, setter(strip_option))]
    position: Option<i64>,
    #[builder(default, setter(strip_option))]
    batch_size: Option<i64>,
    #[builder(default, setter(strip_option))]
    condition: Option<&'a str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, TypedBuilder)]
pub struct GetCategoryMessagesOpts<'a> {
    #[builder(default, setter(strip_option))]
    pub(crate) position: Option<i64>,
    #[builder(default, setter(strip_option))]
    pub(crate) batch_size: Option<i64>,
    #[builder(default, setter(strip_option))]
    pub(crate) correlation: Option<&'a str>,
    #[builder(default, setter(strip_option))]
    pub(crate) consumer_group_member: Option<i64>,
    #[builder(default, setter(strip_option))]
    pub(crate) consumer_group_size: Option<i64>,
    #[builder(default, setter(strip_option))]
    pub(crate) condition: Option<&'a str>,
}

impl MessageDb {
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(MessageDb {
            pool: PgPool::connect(url).await?,
        })
    }

    pub fn transaction<'a, F, R>(&'a self, callback: F) -> BoxFuture<'a, Result<R>>
    where
        for<'c> F:
            'a + FnOnce(&'c mut Transaction<'static, Postgres>) -> BoxFuture<'c, Result<R>> + Send,
    {
        async move {
            let mut tx = self.pool.begin().await?;
            callback(&mut tx).await
        }
        .boxed()
    }

    /// Write a JSON-formatted message to a named stream, optionally specifying
    /// JSON-formatted metadata and an expected version number.
    ///
    /// Returns the position of the message written.
    ///
    /// See <http://docs.eventide-project.org/user-guide/message-db/server-functions.html#write-a-message>
    pub async fn write_message<'e, 'c: 'e, E>(
        executor: E,
        stream_name: &str,
        msg_type: &str,
        data: &Value,
        opts: &WriteMessageOpts,
    ) -> Result<i64>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let id = opts
            .id
            .as_ref()
            .map(Cow::Borrowed)
            .unwrap_or_else(|| Cow::Owned(Uuid::new_v4().to_string()));

        let metadata = opts
            .metadata
            .clone()
            .map(serde_json::to_value)
            .transpose()
            .map_err(Error::DeserializeMetadata)?;

        let pos = sqlx::query_scalar!(
            "SELECT message_store.write_message($1, $2, $3, $4, $5, $6)",
            id.as_str(),
            stream_name,
            msg_type,
            data,
            metadata,
            opts.expected_version,
        )
        .fetch_one(executor)
        .await?
        .ok_or(Error::Decode {
            expected: "position version",
        })?;

        Ok(pos)
    }

    /// Retrieve messages from a single stream, optionally specifying the
    /// starting position, the number of messages to retrieve, and an
    /// additional condition that will be appended to the SQL command's
    /// WHERE clause.
    ///
    /// See <http://docs.eventide-project.org/user-guide/message-db/server-functions.html#get-messages-from-a-stream>
    pub async fn get_stream_messages<'e, 'c: 'e, E, T>(
        executor: E,
        stream_name: &str,
        opts: &GetStreamMessagesOpts<'_>,
    ) -> Result<Vec<Message<T>>>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
        T: for<'de> Deserialize<'de>,
    {
        let messages: Vec<GenericMessage> = sqlx::query_as(message_db_fn!(
            "message_store.get_stream_messages($1, $2, $3, $4)"
        ))
        .bind(stream_name)
        .bind(opts.position)
        .bind(opts.batch_size)
        .bind(opts.condition)
        .fetch_all(executor)
        .await?;

        messages.deserialize_messages()
    }

    /// Retrieve messages from a category of streams, optionally specifying the
    /// starting position, the number of messages to retrieve, the
    /// correlation category for Pub/Sub, consumer group parameters,
    /// and an additional condition that will be appended to the SQL command's
    /// WHERE clause.
    ///
    /// See <http://docs.eventide-project.org/user-guide/message-db/server-functions.html#get-messages-from-a-stream>
    pub async fn get_category_messages<'e, 'c: 'e, E, T>(
        executor: E,
        category_name: &str,
        opts: &GetCategoryMessagesOpts<'_>,
    ) -> Result<Vec<Message<T>>>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
        T: for<'de> Deserialize<'de>,
    {
        let messages: Vec<GenericMessage> = sqlx::query_as(message_db_fn!(
            "message_store.get_category_messages($1, $2, $3, $4, $5, $6, $7)"
        ))
        .bind(category_name)
        .bind(opts.position)
        .bind(opts.batch_size)
        .bind(opts.correlation)
        .bind(opts.consumer_group_member)
        .bind(opts.consumer_group_size)
        .bind(opts.condition)
        .fetch_all(executor)
        .await?;

        messages.deserialize_messages()
    }

    /// Retrieves a message messages table that corresponds to the highest
    /// position number in the stream, and (optionally) corresponds to the
    /// message type specified by the type parameter.
    pub async fn get_last_stream_message<'e, 'c: 'e, E, T>(
        executor: E,
        stream_name: &str,
        msg_type: Option<&str>,
    ) -> Result<Option<Message<T>>>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
        T: for<'de> Deserialize<'de>,
    {
        let message: Option<GenericMessage> = sqlx::query_as(message_db_fn!(
            "message_store.get_last_stream_message($1, $2)"
        ))
        .bind(stream_name)
        .bind(msg_type)
        .fetch_optional(executor)
        .await?;

        message.deserialize_messages()
    }

    /// Returns the highest position number in the stream.
    pub async fn stream_version<'e, 'c: 'e, E>(
        executor: E,
        stream_name: &str,
    ) -> Result<Option<i64>>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let version = sqlx::query_scalar!(
            "SELECT * FROM message_store.stream_version($1)",
            stream_name
        )
        .fetch_one(executor)
        .await?;

        Ok(version)
    }

    /// Returns the ID part of the stream name.
    pub async fn id<'e, 'c: 'e, E>(executor: E, stream_name: &str) -> Result<String>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let id = sqlx::query_scalar!("SELECT * FROM message_store.id($1)", stream_name)
            .fetch_one(executor)
            .await?
            .unwrap_or_default();

        Ok(id)
    }

    /// Returns the cardinal ID part of the stream name.
    pub async fn cardinal_id<'e, 'c: 'e, E>(executor: E, stream_name: &str) -> Result<String>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let id = sqlx::query_scalar!("SELECT * FROM message_store.cardinal_id($1)", stream_name)
            .fetch_one(executor)
            .await?
            .unwrap_or_default();

        Ok(id)
    }

    /// Returns the category part of the stream name.
    pub async fn category<'e, 'c: 'e, E>(executor: E, stream_name: &str) -> Result<String>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let category = sqlx::query_scalar!("SELECT * FROM message_store.category($1)", stream_name)
            .fetch_one(executor)
            .await?
            .unwrap_or_default();

        Ok(category)
    }

    /// Returns a boolean affirmative if the stream name is a category.
    pub async fn is_category<'e, 'c: 'e, E>(executor: E, stream_name: &str) -> Result<bool>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let is_category =
            sqlx::query_scalar!("SELECT * FROM message_store.is_category($1)", stream_name)
                .fetch_one(executor)
                .await?
                .unwrap_or_default();

        Ok(is_category)
    }

    /// An [exclusive, transaction-level advisory lock](https://www.postgresql.org/docs/current/functions-admin.html#FUNCTIONS-ADVISORY-LOCKS)
    /// is acquired when a message is written to the stream. The advisory lock
    /// ensures that writes are processed sequentially.
    ///
    /// The lock ID is derived from the category name of the stream being
    /// written to. The result of which is that all writes to streams in a
    /// given category are queued and processed in sequence. This ensures
    /// that write of a message to a stream does not complete after a consumer
    /// has already proceeded past its position.
    ///
    /// Returns an integer representing the lock ID.
    pub async fn acquire_lock<'e, 'c: 'e, E>(executor: E, stream_name: &str) -> Result<i64>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let lock = sqlx::query_scalar!("SELECT * FROM message_store.acquire_lock($1)", stream_name)
            .fetch_one(executor)
            .await?
            .ok_or(Error::Decode {
                expected: "lock id",
            })?;

        Ok(lock)
    }

    /// The lock ID generated to acquire an exclusive advisory lock is a hash
    /// calculated based on the stream name.
    ///
    /// Returns an integer representing the lock ID.
    pub async fn hash_64<'e, 'c: 'e, E>(executor: E, value: &str) -> Result<i64>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let hash = sqlx::query_scalar!("SELECT * FROM message_store.hash_64($1)", value)
            .fetch_one(executor)
            .await?
            .ok_or(Error::Decode {
                expected: "hash 64",
            })?;

        Ok(hash)
    }

    /// The lock ID generated to acquire an exclusive advisory lock is a hash
    /// calculated based on the stream name.
    ///
    /// Returns an integer representing the lock ID.
    pub async fn message_store_version<'e, 'c: 'e, E>(executor: E) -> Result<String>
    where
        E: 'e + sqlx::Executor<'c, Database = Postgres>,
    {
        let version = sqlx::query_scalar!("SELECT * FROM message_store.message_store_version()")
            .fetch_one(executor)
            .await?
            .ok_or(Error::Decode {
                expected: "message store version",
            })?;

        Ok(version)
    }
}

impl<'c> Executor<'c> for &MessageDb {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E: 'q>(
        self,
        query: E,
    ) -> futures::stream::BoxStream<
        'e,
        Result<
            either::Either<
                <Self::Database as sqlx::Database>::QueryResult,
                <Self::Database as sqlx::Database>::Row,
            >,
            sqlx::Error,
        >,
    >
    where
        'c: 'e,
        E: sqlx::Execute<'q, Self::Database>,
    {
        self.pool.fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, E: 'q>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<<Self::Database as sqlx::Database>::Row>, sqlx::Error>>
    where
        'c: 'e,
        E: sqlx::Execute<'q, Self::Database>,
    {
        self.pool.fetch_optional(query)
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [<Self::Database as sqlx::Database>::TypeInfo],
    ) -> BoxFuture<
        'e,
        Result<<Self::Database as sqlx::database::HasStatement<'q>>::Statement, sqlx::Error>,
    >
    where
        'c: 'e,
    {
        self.pool.prepare_with(sql, parameters)
    }

    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> BoxFuture<'e, Result<sqlx::Describe<Self::Database>, sqlx::Error>>
    where
        'c: 'e,
    {
        self.pool.describe(sql)
    }
}