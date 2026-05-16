use super::*;

#[derive(Clone)]
pub struct UserStore {
    pub pool: SqlitePool,
}

impl UserStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn upsert(
        &self,
        telegram_user_id: i64,
        username: Option<&str>,
        first_name: Option<&str>,
        last_name: Option<&str>,
        is_group_member: bool,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let row = sqlx::query(
            r#"
        INSERT INTO users (
            id, telegram_user_id, telegram_username, first_name, last_name,
            is_group_member, last_membership_checked_at, updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, datetime('now'), datetime('now'))
        ON CONFLICT (telegram_user_id) DO UPDATE SET
            telegram_username = EXCLUDED.telegram_username,
            first_name = EXCLUDED.first_name,
            last_name = EXCLUDED.last_name,
            is_group_member = EXCLUDED.is_group_member,
            last_membership_checked_at = datetime('now'),
            updated_at = datetime('now')
        RETURNING id
        "#,
        )
        .bind(uuid_value(id))
        .bind(telegram_user_id)
        .bind(username)
        .bind(first_name)
        .bind(last_name)
        .bind(is_group_member)
        .fetch_one(&self.pool)
        .await?;

        uuid_from_row(&row, "id")
    }
}
