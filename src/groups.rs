//! Groups of users. A group holds users and other groups (nested groups); a user, or a
//! group, can be in several groups. Groups carry no right yet.
//!
//! * Only admins see and manage groups.
//! * A user only sees the groups they belong to, directly or through a nested group
//!   (`GET /api/me/groups`), and can change nothing.
//!
//! Cycles are refused: a group never ends up inside itself, so walking down the subgroups
//! of a group always ends.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tokio::sync::Mutex;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    audit,
    auth::{AuthUser, now},
    errors::ApiError,
    models::Message,
    state::SharedState,
};

const MAX_NAME_LEN: usize = 64;
const MAX_DESCRIPTION_LEN: usize = 500;

/// "Check there is no cycle, then link" must not interleave with another link.
static TREE_LOCK: Mutex<()> = Mutex::const_new(());

/// Every group under ?1, itself included, following the subgroups.
const SUBTREE: &str = "WITH RECURSIVE tree(id) AS (
    SELECT ?1 UNION SELECT s.child_id FROM user_group_subgroups s JOIN tree t ON s.group_id = t.id)";

const GROUP_SELECT: &str = "SELECT g.id, g.name, g.description,
    (SELECT COUNT(*) FROM user_group_members m WHERE m.group_id = g.id) AS member_count,
    (SELECT COUNT(*) FROM user_group_subgroups s WHERE s.group_id = g.id) AS subgroup_count,
    g.created_at, g.updated_at
    FROM user_groups g";

#[derive(Serialize, ToSchema, FromRow)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Users directly in the group.
    pub member_count: i64,
    /// Groups directly in the group.
    pub subgroup_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A group with its direct content and the groups it is directly in.
#[derive(Serialize, ToSchema)]
pub struct GroupDetail {
    pub id: String,
    pub name: String,
    pub description: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub members: Vec<GroupMember>,
    pub subgroups: Vec<GroupRef>,
    /// Groups this group is directly in.
    pub parents: Vec<GroupRef>,
}

#[derive(Serialize, ToSchema, FromRow)]
pub struct GroupMember {
    pub user_id: String,
    pub username: String,
    pub added_at: i64,
}

#[derive(Serialize, ToSchema, FromRow)]
pub struct GroupRef {
    pub id: String,
    pub name: String,
    pub added_at: i64,
}

/// A user of a group, nested groups included.
#[derive(Serialize, ToSchema, FromRow)]
pub struct EffectiveMember {
    pub user_id: String,
    pub username: String,
    /// True if the user is in the group itself, false if only through a subgroup.
    pub direct: bool,
}

/// A group a user belongs to.
#[derive(Serialize, ToSchema, FromRow)]
pub struct UserGroup {
    pub id: String,
    pub name: String,
    pub description: String,
    /// True if the user is in the group itself, false if only through a nested group.
    pub direct: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateGroup {
    pub name: String,
    pub description: Option<String>,
}

/// Fields left out keep their value.
#[derive(Deserialize, ToSchema)]
pub struct UpdateGroup {
    pub name: Option<String>,
    pub description: Option<String>,
}

// ---------- groups ----------

#[utoipa::path(get, path = "/api/groups", responses((status = 200, body = [Group]), (status = 403, body = Message)), tag = "Groups")]
pub async fn list(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<Group>>, ApiError> {
    user.require_admin()?;
    let groups = sqlx::query_as::<_, Group>(&format!("{GROUP_SELECT} ORDER BY g.name"))
        .fetch_all(&state.db)
        .await?;
    Ok(Json(groups))
}

#[utoipa::path(post, path = "/api/groups", request_body = CreateGroup, responses((status = 201, body = Group), (status = 400, body = Message), (status = 403, body = Message), (status = 409, body = Message, description = "A group with this name already exists")), tag = "Groups")]
pub async fn create(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<CreateGroup>,
) -> Result<(StatusCode, Json<Group>), ApiError> {
    user.require_admin()?;
    let name = validate_name(&body.name)?;
    let description = validate_description(body.description.as_deref().unwrap_or(""))?;
    let id = Uuid::new_v4().to_string();
    let created_at = now();
    sqlx::query(
        "INSERT INTO user_groups (id, name, description, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&description)
    .bind(created_at)
    .bind(created_at)
    .execute(&state.db)
    .await
    .map_err(name_conflict)?;
    audit::record(&state, &user, "group.create", &id, &name).await;
    Ok((StatusCode::CREATED, Json(load(&state, &id).await?)))
}

#[utoipa::path(get, path = "/api/groups/{group_id}", params(("group_id" = String, Path)), responses((status = 200, body = GroupDetail), (status = 403, body = Message), (status = 404, body = Message)), tag = "Groups")]
pub async fn get(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(group_id): Path<String>,
) -> Result<Json<GroupDetail>, ApiError> {
    user.require_admin()?;
    let group = load(&state, &group_id).await?;
    let members = sqlx::query_as::<_, GroupMember>(
        "SELECT m.user_id, u.username, m.created_at AS added_at
         FROM user_group_members m JOIN users u ON u.id = m.user_id
         WHERE m.group_id = ? ORDER BY u.username",
    )
    .bind(&group.id)
    .fetch_all(&state.db)
    .await?;
    let subgroups = sqlx::query_as::<_, GroupRef>(
        "SELECT g.id, g.name, s.created_at AS added_at
         FROM user_group_subgroups s JOIN user_groups g ON g.id = s.child_id
         WHERE s.group_id = ? ORDER BY g.name",
    )
    .bind(&group.id)
    .fetch_all(&state.db)
    .await?;
    let parents = sqlx::query_as::<_, GroupRef>(
        "SELECT g.id, g.name, s.created_at AS added_at
         FROM user_group_subgroups s JOIN user_groups g ON g.id = s.group_id
         WHERE s.child_id = ? ORDER BY g.name",
    )
    .bind(&group.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(GroupDetail {
        id: group.id,
        name: group.name,
        description: group.description,
        created_at: group.created_at,
        updated_at: group.updated_at,
        members,
        subgroups,
        parents,
    }))
}

#[utoipa::path(patch, path = "/api/groups/{group_id}", params(("group_id" = String, Path)), request_body = UpdateGroup, responses((status = 200, body = Group), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Groups")]
pub async fn update(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(group_id): Path<String>,
    Json(body): Json<UpdateGroup>,
) -> Result<Json<Group>, ApiError> {
    user.require_admin()?;
    let group = load(&state, &group_id).await?;
    let name = match &body.name {
        Some(name) => validate_name(name)?,
        None => group.name,
    };
    let description = match &body.description {
        Some(description) => validate_description(description)?,
        None => group.description,
    };
    sqlx::query("UPDATE user_groups SET name = ?, description = ?, updated_at = ? WHERE id = ?")
        .bind(&name)
        .bind(&description)
        .bind(now())
        .bind(&group.id)
        .execute(&state.db)
        .await
        .map_err(name_conflict)?;
    audit::record(&state, &user, "group.update", &group.id, &name).await;
    Ok(Json(load(&state, &group.id).await?))
}

/// The members and subgroups are not deleted, only their link to this group.
#[utoipa::path(delete, path = "/api/groups/{group_id}", params(("group_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message)), tag = "Groups")]
pub async fn delete(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(group_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let group = load(&state, &group_id).await?;
    sqlx::query("DELETE FROM user_groups WHERE id = ?")
        .bind(&group.id)
        .execute(&state.db)
        .await?;
    audit::record(&state, &user, "group.delete", &group.id, &group.name).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- members ----------

/// Every user of the group, through its subgroups too.
#[utoipa::path(get, path = "/api/groups/{group_id}/members", params(("group_id" = String, Path)), responses((status = 200, body = [EffectiveMember], description = "Users of the group and of all its nested groups")), tag = "Groups")]
pub async fn members(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(group_id): Path<String>,
) -> Result<Json<Vec<EffectiveMember>>, ApiError> {
    user.require_admin()?;
    let group = load(&state, &group_id).await?;
    let members = sqlx::query_as::<_, EffectiveMember>(&format!(
        "{SUBTREE}
         SELECT u.id AS user_id, u.username, MAX(m.group_id = ?1) AS direct
         FROM tree t JOIN user_group_members m ON m.group_id = t.id JOIN users u ON u.id = m.user_id
         GROUP BY u.id ORDER BY u.username"
    ))
    .bind(&group.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(members))
}

#[utoipa::path(put, path = "/api/groups/{group_id}/members/{user_id}", params(("group_id" = String, Path), ("user_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message)), tag = "Groups")]
pub async fn add_member(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((group_id, user_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let group = load(&state, &group_id).await?;
    let username: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE id = ?")
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await?;
    let username = username.ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    sqlx::query(
        "INSERT OR IGNORE INTO user_group_members (group_id, user_id, created_at) VALUES (?, ?, ?)",
    )
    .bind(&group.id)
    .bind(&user_id)
    .bind(now())
    .execute(&state.db)
    .await?;
    audit::record(&state, &user, "group.add_member", &group.id, &username).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/groups/{group_id}/members/{user_id}", params(("group_id" = String, Path), ("user_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message)), tag = "Groups")]
pub async fn remove_member(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((group_id, user_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let group = load(&state, &group_id).await?;
    let result = sqlx::query("DELETE FROM user_group_members WHERE group_id = ? AND user_id = ?")
        .bind(&group.id)
        .bind(&user_id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("The user is not in this group".into()));
    }
    audit::record(&state, &user, "group.remove_member", &group.id, &user_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- subgroups ----------

/// Puts `child_id` inside the group. Refused if the group is already inside `child_id`.
#[utoipa::path(put, path = "/api/groups/{group_id}/subgroups/{child_id}", params(("group_id" = String, Path), ("child_id" = String, Path)), responses((status = 204), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message, description = "This would put the group inside itself")), tag = "Groups")]
pub async fn add_subgroup(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((group_id, child_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let group = load(&state, &group_id).await?;
    let child = load(&state, &child_id).await?;
    if group.id == child.id {
        return Err(ApiError::BadRequest("A group cannot contain itself".into()));
    }
    {
        let _tree = TREE_LOCK.lock().await;
        let cycle: Option<i64> =
            sqlx::query_scalar(&format!("{SUBTREE} SELECT 1 FROM tree WHERE id = ?2"))
                .bind(&child.id)
                .bind(&group.id)
                .fetch_optional(&state.db)
                .await?;
        if cycle.is_some() {
            return Err(ApiError::Conflict(format!(
                "{} is already inside {}: adding it would put the group inside itself",
                group.name, child.name
            )));
        }
        sqlx::query(
            "INSERT OR IGNORE INTO user_group_subgroups (group_id, child_id, created_at) VALUES (?, ?, ?)",
        )
        .bind(&group.id)
        .bind(&child.id)
        .bind(now())
        .execute(&state.db)
        .await?;
    }
    audit::record(&state, &user, "group.add_subgroup", &group.id, &child.name).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/groups/{group_id}/subgroups/{child_id}", params(("group_id" = String, Path), ("child_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message)), tag = "Groups")]
pub async fn remove_subgroup(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((group_id, child_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let group = load(&state, &group_id).await?;
    let result =
        sqlx::query("DELETE FROM user_group_subgroups WHERE group_id = ? AND child_id = ?")
            .bind(&group.id)
            .bind(&child_id)
            .execute(&state.db)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(
            "This group is not a subgroup of the group".into(),
        ));
    }
    audit::record(&state, &user, "group.remove_subgroup", &group.id, &child_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- groups of a user ----------

/// The groups the caller is in, directly or through nested groups. Read only.
#[utoipa::path(get, path = "/api/me/groups", responses((status = 200, body = [UserGroup]), (status = 401, body = Message)), tag = "Groups")]
pub async fn my_groups(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<UserGroup>>, ApiError> {
    Ok(Json(groups_of(&state, &user.id).await?))
}

#[utoipa::path(get, path = "/api/users/{user_id}/groups", params(("user_id" = String, Path)), responses((status = 200, body = [UserGroup]), (status = 403, body = Message), (status = 404, body = Message)), tag = "Groups")]
pub async fn user_groups(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(user_id): Path<String>,
) -> Result<Json<Vec<UserGroup>>, ApiError> {
    user.require_admin()?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?")
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await?;
    exists.ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    Ok(Json(groups_of(&state, &user_id).await?))
}

/// Groups containing the user, then the groups containing those, and so on.
pub async fn groups_of(state: &SharedState, user_id: &str) -> Result<Vec<UserGroup>, ApiError> {
    Ok(sqlx::query_as::<_, UserGroup>(
        "WITH RECURSIVE mine(id, direct) AS (
             SELECT group_id, 1 FROM user_group_members WHERE user_id = ?1
             UNION SELECT s.group_id, 0 FROM user_group_subgroups s JOIN mine m ON s.child_id = m.id)
         SELECT g.id, g.name, g.description, MAX(m.direct) AS direct
         FROM mine m JOIN user_groups g ON g.id = m.id
         GROUP BY g.id ORDER BY g.name",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?)
}

// ---------- helpers ----------

async fn load(state: &SharedState, group_id: &str) -> Result<Group, ApiError> {
    sqlx::query_as::<_, Group>(&format!("{GROUP_SELECT} WHERE g.id = ?"))
        .bind(group_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Group not found".into()))
}

fn validate_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest(format!(
            "name must be 1 to {MAX_NAME_LEN} printable characters"
        )));
    }
    Ok(name.to_owned())
}

fn validate_description(description: &str) -> Result<String, ApiError> {
    let description = description.trim();
    if description.chars().count() > MAX_DESCRIPTION_LEN
        || description
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ApiError::BadRequest(format!(
            "description must be at most {MAX_DESCRIPTION_LEN} printable characters"
        )));
    }
    Ok(description.to_owned())
}

fn name_conflict(error: sqlx::Error) -> ApiError {
    match error {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            ApiError::Conflict("A group with this name already exists".into())
        }
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::{Value, json};

    use super::*;
    use crate::http_tests::{TestApp, unique};

    async fn create_group(app: &TestApp, name: &str) -> String {
        let (status, _, json) = app
            .send(Method::POST, "/api/groups", Some(json!({ "name": name })))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{json}");
        json["id"].as_str().unwrap().to_owned()
    }

    async fn put(app: &TestApp, path: &str) -> (StatusCode, Value) {
        app.status_of(Method::PUT, path).await
    }

    fn names(json: &Value) -> Vec<&str> {
        json.as_array()
            .unwrap()
            .iter()
            .map(|g| g["name"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn names_and_descriptions_are_checked() {
        assert_eq!(validate_name("  Équipe réseau ").unwrap(), "Équipe réseau");
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a\nb").is_err());
        assert!(validate_name(&"x".repeat(MAX_NAME_LEN + 1)).is_err());
        assert!(validate_description("line 1\nline 2").is_ok());
        assert!(validate_description(&"x".repeat(MAX_DESCRIPTION_LEN + 1)).is_err());
    }

    #[tokio::test]
    async fn admins_create_edit_and_delete_groups() {
        let app = TestApp::new().await;
        let name = unique("devs");
        let (status, _, group) = app
            .send(
                Method::POST,
                "/api/groups",
                Some(json!({ "name": name, "description": "Developers" })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{group}");
        assert_eq!(group["member_count"], 0);
        let id = group["id"].as_str().unwrap();
        let path = format!("/api/groups/{id}");

        let (status, _, _) = app
            .send(
                Method::POST,
                "/api/groups",
                Some(json!({ "name": name.to_uppercase() })),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "names are case-insensitive");

        let renamed = unique("team");
        let (status, _, group) = app
            .send(Method::PATCH, &path, Some(json!({ "name": renamed })))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(group["name"], renamed.as_str());
        assert_eq!(group["description"], "Developers");

        let (_, listed) = app.status_of(Method::GET, "/api/groups").await;
        assert_eq!(names(&listed), vec![renamed.as_str()]);

        let (status, _) = app.status_of(Method::DELETE, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = app.status_of(Method::GET, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn users_cannot_see_nor_touch_groups() {
        let app = TestApp::new().await;
        let id = create_group(&app, &unique("g")).await;
        let other = create_group(&app, &unique("g")).await;
        let (user_id, token) = app.user(&unique("user"), "user").await;
        let group = format!("/api/groups/{id}");
        for (method, path, body) in [
            (Method::GET, "/api/groups".to_owned(), None),
            (
                Method::POST,
                "/api/groups".to_owned(),
                Some(json!({ "name": "x" })),
            ),
            (Method::GET, group.clone(), None),
            (Method::PATCH, group.clone(), Some(json!({ "name": "x" }))),
            (Method::DELETE, group.clone(), None),
            (Method::GET, format!("{group}/members"), None),
            (Method::PUT, format!("{group}/members/{user_id}"), None),
            (Method::DELETE, format!("{group}/members/{user_id}"), None),
            (Method::PUT, format!("{group}/subgroups/{other}"), None),
            (Method::DELETE, format!("{group}/subgroups/{other}"), None),
            (Method::GET, format!("/api/users/{user_id}/groups"), None),
        ] {
            let (status, _, _) = app.send_as(Some(&token), method.clone(), &path, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }
        let (status, _, _) = app.send_as(None, Method::GET, "/api/me/groups", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn groups_nest_and_a_user_can_be_in_several() {
        let app = TestApp::new().await;
        // company > it > network ; alice in network and in sport
        let company = create_group(&app, &unique("company")).await;
        let it = create_group(&app, &unique("it")).await;
        let network = create_group(&app, &unique("network")).await;
        let sport = create_group(&app, &unique("sport")).await;
        let (alice, token) = app.user(&unique("alice"), "user").await;
        let (bob, _) = app.user(&unique("bob"), "user").await;

        for path in [
            format!("/api/groups/{company}/subgroups/{it}"),
            format!("/api/groups/{it}/subgroups/{network}"),
            format!("/api/groups/{network}/members/{alice}"),
            format!("/api/groups/{sport}/members/{alice}"),
            format!("/api/groups/{company}/members/{bob}"),
        ] {
            let (status, json) = put(&app, &path).await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{path} {json}");
        }
        // Adding twice changes nothing.
        let (status, _) = put(&app, &format!("/api/groups/{sport}/members/{alice}")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, mine) = app.status_as(&token, Method::GET, "/api/me/groups").await;
        assert_eq!(status, StatusCode::OK);
        let mine = mine.as_array().unwrap();
        assert_eq!(
            mine.len(),
            4,
            "network, sport, and through network: it, company"
        );
        let direct = |id: &str| {
            mine.iter()
                .find(|g| g["id"] == id)
                .map(|g| g["direct"].as_bool().unwrap())
        };
        assert_eq!(direct(&network), Some(true));
        assert_eq!(direct(&sport), Some(true));
        assert_eq!(direct(&it), Some(false));
        assert_eq!(direct(&company), Some(false));

        let (_, as_admin) = app
            .status_of(Method::GET, &format!("/api/users/{alice}/groups"))
            .await;
        assert_eq!(as_admin.as_array().unwrap().len(), 4);

        let (_, members) = app
            .status_of(Method::GET, &format!("/api/groups/{company}/members"))
            .await;
        let members = members.as_array().unwrap();
        assert_eq!(members.len(), 2);
        let of = |id: &str| members.iter().find(|m| m["user_id"] == id).unwrap()["direct"].clone();
        assert_eq!(of(&bob), true);
        assert_eq!(of(&alice), false);

        let (_, detail) = app
            .status_of(Method::GET, &format!("/api/groups/{it}"))
            .await;
        assert_eq!(detail["subgroups"][0]["id"], network.as_str());
        assert_eq!(detail["parents"][0]["id"], company.as_str());
        assert!(detail["members"].as_array().unwrap().is_empty());

        // Taking network out of it: alice is no longer in it nor company.
        let (status, _) = app
            .status_of(
                Method::DELETE,
                &format!("/api/groups/{it}/subgroups/{network}"),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, mine) = app.status_as(&token, Method::GET, "/api/me/groups").await;
        assert_eq!(mine.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_group_never_ends_up_inside_itself() {
        let app = TestApp::new().await;
        let a = create_group(&app, &unique("a")).await;
        let b = create_group(&app, &unique("b")).await;
        let c = create_group(&app, &unique("c")).await;
        put(&app, &format!("/api/groups/{a}/subgroups/{b}")).await;
        put(&app, &format!("/api/groups/{b}/subgroups/{c}")).await;

        let (status, _) = put(&app, &format!("/api/groups/{a}/subgroups/{a}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        for (parent, child) in [(&c, &a), (&b, &a), (&c, &b)] {
            let (status, json) =
                put(&app, &format!("/api/groups/{parent}/subgroups/{child}")).await;
            assert_eq!(status, StatusCode::CONFLICT, "{json}");
        }
        // A diamond is not a cycle: a > b > c and a > c.
        let (status, _) = put(&app, &format!("/api/groups/{a}/subgroups/{c}")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn deleting_a_group_or_a_user_only_removes_links() {
        let app = TestApp::new().await;
        let parent = create_group(&app, &unique("parent")).await;
        let child = create_group(&app, &unique("child")).await;
        let (user, token) = app.user(&unique("carol"), "user").await;
        put(&app, &format!("/api/groups/{parent}/subgroups/{child}")).await;
        put(&app, &format!("/api/groups/{child}/members/{user}")).await;

        let (status, _) = app
            .status_of(Method::DELETE, &format!("/api/groups/{parent}"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, detail) = app
            .status_of(Method::GET, &format!("/api/groups/{child}"))
            .await;
        assert_eq!(status, StatusCode::OK, "the subgroup is kept");
        assert!(detail["parents"].as_array().unwrap().is_empty());
        let (_, mine) = app.status_as(&token, Method::GET, "/api/me/groups").await;
        assert_eq!(names(&mine).len(), 1);

        let (status, _) = app
            .status_of(Method::DELETE, &format!("/api/users/{user}"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, detail) = app
            .status_of(Method::GET, &format!("/api/groups/{child}"))
            .await;
        assert!(detail["members"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_users_and_groups_are_reported() {
        let app = TestApp::new().await;
        let group = create_group(&app, &unique("g")).await;
        let missing = Uuid::new_v4();
        for path in [
            format!("/api/groups/{group}/members/{missing}"),
            format!("/api/groups/{group}/subgroups/{missing}"),
            format!("/api/groups/{missing}/members/{missing}"),
        ] {
            let (status, _) = put(&app, &path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
        let (status, _) = app
            .status_of(
                Method::DELETE,
                &format!("/api/groups/{group}/members/{missing}"),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = app
            .status_of(Method::GET, &format!("/api/users/{missing}/groups"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
