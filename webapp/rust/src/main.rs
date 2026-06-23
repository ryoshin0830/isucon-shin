use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use axum::{
    body::Bytes,
    extract::{Extension, Form, Multipart, Path, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use askama::Template;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha512};
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::{Executor, MySqlPool};
use tower::ServiceExt;
use tower_http::services::ServeDir;

const POSTS_PER_PAGE: usize = 20;
// fetch a buffer larger than POSTS_PER_PAGE so app-side del_flg filtering still yields 20.
// (~2% of users are banned, so 60 is a very safe margin)
const POSTS_FETCH_LIMIT: usize = 60;
const UPLOAD_LIMIT: usize = 10 * 1024 * 1024;
const PUBLIC_DIR: &str = "/home/isucon/private_isu/webapp/public";
const IMAGE_DIR: &str = "/home/isucon/private_isu/webapp/public/image";

fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        _ => "",
    }
}

// write image to public/image/<id>.<ext> so nginx can serve it directly afterwards.
// write to a temp file then atomically rename, so nginx never serves a partially-written
// file (which triggered "sendfile() reported that ... was truncated" under high POST load).
async fn write_image_file(id: i64, mime: &str, data: &[u8]) {
    let ext = ext_for_mime(mime);
    if ext.is_empty() {
        return;
    }
    let path = format!("{}/{}.{}", IMAGE_DIR, id, ext);
    let tmp = format!("{}/.{}.{}.tmp", IMAGE_DIR, id, ext);
    if let Err(e) = tokio::fs::write(&tmp, data).await {
        eprintln!("write image tmp {tmp} failed: {e}");
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
        eprintln!("rename image {tmp} -> {path} failed: {e}");
        let _ = tokio::fs::remove_file(&tmp).await;
    }
}

// ---------- models ----------

#[derive(Clone, sqlx::FromRow)]
struct User {
    id: i32,
    account_name: String,
    #[allow(dead_code)]
    passhash: String,
    authority: i8,
    del_flg: i8,
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
}

impl User {
    fn empty() -> Self {
        User {
            id: 0,
            account_name: String::new(),
            passhash: String::new(),
            authority: 0,
            del_flg: 0,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostRow {
    id: i32,
    user_id: i32,
    body: String,
    mime: String,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct CommentRow {
    #[allow(dead_code)]
    id: i32,
    #[allow(dead_code)]
    post_id: i32,
    user_id: i32,
    comment: String,
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
}

struct Comment {
    comment: String,
    user: User,
}

struct Post {
    id: i32,
    user: User,
    body: String,
    mime: String,
    created_at: DateTime<Utc>,
    comment_count: i64,
    comments: Vec<Comment>,
    csrf_token: String,
}

impl Post {
    fn image_url(&self) -> String {
        let ext = match self.mime.as_str() {
            "image/jpeg" => ".jpg",
            "image/png" => ".png",
            "image/gif" => ".gif",
            _ => "",
        };
        format!("/image/{}{}", self.id, ext)
    }
    fn created_at_iso(&self) -> String {
        self.created_at.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    }
}

// ---------- templates ----------

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    me: User,
    posts: Vec<Post>,
    csrf_token: String,
    flash: String,
}

#[derive(Template)]
#[template(path = "posts.html")]
struct PostsTemplate {
    posts: Vec<Post>,
}

#[derive(Template)]
#[template(path = "post_id.html")]
struct PostIdTemplate {
    me: User,
    post: Post,
}

#[derive(Template)]
#[template(path = "user.html")]
struct UserTemplate {
    me: User,
    user: User,
    posts: Vec<Post>,
    post_count: i64,
    comment_count: i64,
    commented_count: i64,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    me: User,
    flash: String,
}

#[derive(Template)]
#[template(path = "register.html")]
struct RegisterTemplate {
    me: User,
    flash: String,
}

#[derive(Template)]
#[template(path = "banned.html")]
struct BannedTemplate {
    me: User,
    users: Vec<User>,
    csrf_token: String,
}

// ---------- session ----------

#[derive(Clone, Default)]
struct SessionData {
    user_id: Option<i32>,
    csrf_token: Option<String>,
    notice: Option<String>,
}

type SessionStore = Arc<Mutex<HashMap<String, SessionData>>>;

#[derive(Clone)]
struct SessionId(String);

// In-memory cache of all users. Users are read on nearly every request (current_user,
// post authors, comment authors) but change rarely (register / ban / initialize), so we
// keep them in memory and avoid hitting MySQL for user lookups entirely.
#[derive(Default)]
struct UserCache {
    by_id: HashMap<i32, User>,
    by_name: HashMap<String, i32>,
}

type UserStore = Arc<RwLock<UserCache>>;

#[derive(Clone)]
struct AppState {
    db: MySqlPool,
    sessions: SessionStore,
    users: UserStore,
}

async fn load_user_cache(db: &MySqlPool) -> UserCache {
    let users: Vec<User> = sqlx::query_as("SELECT * FROM `users`")
        .fetch_all(db)
        .await
        .unwrap_or_default();
    let mut by_id = HashMap::with_capacity(users.len());
    let mut by_name = HashMap::with_capacity(users.len());
    for u in users {
        by_name.insert(u.account_name.clone(), u.id);
        by_id.insert(u.id, u);
    }
    UserCache { by_id, by_name }
}

fn cache_user_by_id(state: &AppState, id: i32) -> Option<User> {
    state.users.read().unwrap().by_id.get(&id).cloned()
}

fn cache_user_by_name(state: &AppState, name: &str) -> Option<User> {
    let c = state.users.read().unwrap();
    c.by_name.get(name).and_then(|id| c.by_id.get(id)).cloned()
}

fn cache_insert_user(state: &AppState, u: User) {
    let mut c = state.users.write().unwrap();
    c.by_name.insert(u.account_name.clone(), u.id);
    c.by_id.insert(u.id, u);
}

fn cache_set_del_flg(state: &AppState, id: i32, flg: i8) {
    let mut c = state.users.write().unwrap();
    if let Some(u) = c.by_id.get_mut(&id) {
        u.del_flg = flg;
    }
}

// ---------- helpers ----------

fn rand_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

fn digest(src: &str) -> String {
    let mut h = Sha512::new();
    h.update(src.as_bytes());
    hex::encode(h.finalize())
}

fn calculate_salt(account_name: &str) -> String {
    digest(account_name)
}

fn calculate_passhash(account_name: &str, password: &str) -> String {
    digest(&format!("{}:{}", password, calculate_salt(account_name)))
}

fn validate_user(account_name: &str, password: &str) -> bool {
    let ok_chars = |s: &str| s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    account_name.len() >= 3 && ok_chars(account_name) && password.len() >= 6 && ok_chars(password)
}

fn cookie_value(req: &Request, name: &str) -> Option<String> {
    let header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(&format!("{}=", name)) {
            return Some(rest.to_string());
        }
    }
    None
}

// session field access
fn session_get(state: &AppState, sid: &str) -> SessionData {
    state
        .sessions
        .lock()
        .unwrap()
        .get(sid)
        .cloned()
        .unwrap_or_default()
}

fn session_set<F: FnOnce(&mut SessionData)>(state: &AppState, sid: &str, f: F) {
    let mut map = state.sessions.lock().unwrap();
    let entry = map.entry(sid.to_string()).or_default();
    f(entry);
}

async fn current_user(state: &AppState, sid: &str) -> User {
    let s = session_get(state, sid);
    if let Some(uid) = s.user_id {
        cache_user_by_id(state, uid).unwrap_or_else(User::empty)
    } else {
        User::empty()
    }
}

fn csrf_token(state: &AppState, sid: &str) -> String {
    session_get(state, sid).csrf_token.unwrap_or_default()
}

fn get_flash(state: &AppState, sid: &str, key: &str) -> String {
    // only "notice" is used as a flash
    let _ = key;
    let mut map = state.sessions.lock().unwrap();
    if let Some(s) = map.get_mut(sid) {
        s.notice.take().unwrap_or_default()
    } else {
        String::new()
    }
}

fn is_login(u: &User) -> bool {
    u.id != 0
}

fn redirect(location: &str) -> Response {
    (
        StatusCode::FOUND,
        [(header::LOCATION, HeaderValue::from_str(location).unwrap())],
    )
        .into_response()
}

fn render<T: Template>(t: T) -> Response {
    match t.render() {
        Ok(body) => Html(body).into_response(),
        Err(e) => {
            eprintln!("template error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// error type: log and return 500 (matches "stop processing" behavior)
struct AppErr(String);
impl From<sqlx::Error> for AppErr {
    fn from(e: sqlx::Error) -> Self {
        AppErr(e.to_string())
    }
}
impl From<axum::extract::multipart::MultipartError> for AppErr {
    fn from(e: axum::extract::multipart::MultipartError) -> Self {
        AppErr(e.to_string())
    }
}
impl IntoResponse for AppErr {
    fn into_response(self) -> Response {
        eprintln!("error: {}", self.0);
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

// ---------- core: makePosts ----------

// Batched makePosts: replaces the per-post N+1 (count + comments + per-comment user + author)
// with a small fixed number of IN-queries regardless of post count.
async fn make_posts(
    state: &AppState,
    rows: Vec<PostRow>,
    csrf: &str,
    all_comments: bool,
) -> Result<Vec<Post>, sqlx::Error> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // 1) pick the posts we will actually display: skip banned authors (del_flg via user
    //    cache, no DB) and cap at POSTS_PER_PAGE. Authors come from the in-memory cache.
    let mut display: Vec<(PostRow, User)> = Vec::with_capacity(POSTS_PER_PAGE);
    for r in rows {
        if let Some(author) = cache_user_by_id(state, r.user_id) {
            if author.del_flg == 0 {
                display.push((r, author));
                if display.len() >= POSTS_PER_PAGE {
                    break;
                }
            }
        }
    }
    if display.is_empty() {
        return Ok(Vec::new());
    }

    let post_ids: Vec<i32> = display.iter().map(|(r, _)| r.id).collect();
    let ph = vec!["?"; post_ids.len()].join(",");

    // 2) comment counts for the displayed posts (one query)
    let count_q = format!(
        "SELECT `post_id`, COUNT(*) AS `cnt` FROM `comments` WHERE `post_id` IN ({}) GROUP BY `post_id`",
        ph
    );
    let mut cq = sqlx::query_as::<_, (i32, i64)>(&count_q);
    for id in &post_ids {
        cq = cq.bind(id);
    }
    let count_rows = cq.fetch_all(&state.db).await?;
    let mut count_map: HashMap<i32, i64> = HashMap::with_capacity(count_rows.len());
    for (pid, c) in count_rows {
        count_map.insert(pid, c);
    }

    // 3) comments for the displayed posts (one query). No ORDER BY in SQL (avoids a
    //    cross-post filesort); we sort each post's bucket newest-first in Rust.
    let comments_q = format!(
        "SELECT `id`, `post_id`, `user_id`, `comment`, `created_at` FROM `comments` WHERE `post_id` IN ({})",
        ph
    );
    let mut coq = sqlx::query_as::<_, CommentRow>(&comments_q);
    for id in &post_ids {
        coq = coq.bind(id);
    }
    let comment_rows = coq.fetch_all(&state.db).await?;

    let mut comments_by_post: HashMap<i32, Vec<CommentRow>> = HashMap::new();
    for c in comment_rows {
        comments_by_post.entry(c.post_id).or_default().push(c);
    }
    for bucket in comments_by_post.values_mut() {
        bucket.sort_unstable_by(|a, b| b.created_at.cmp(&a.created_at)); // newest first
    }

    // 4) assemble (comment authors come from the user cache)
    let mut posts: Vec<Post> = Vec::with_capacity(display.len());
    for (r, puser) in display {
        let mut comments: Vec<Comment> = Vec::new();
        if let Some(bucket) = comments_by_post.get(&r.id) {
            let take = if all_comments { bucket.len() } else { bucket.len().min(3) };
            for c in &bucket[..take] {
                if let Some(cu) = cache_user_by_id(state, c.user_id) {
                    comments.push(Comment {
                        comment: c.comment.clone(),
                        user: cu,
                    });
                }
            }
            comments.reverse(); // oldest-first for display (matches original)
        }

        posts.push(Post {
            id: r.id,
            user: puser,
            body: r.body,
            mime: r.mime,
            created_at: r.created_at,
            comment_count: *count_map.get(&r.id).unwrap_or(&0),
            comments,
            csrf_token: csrf.to_string(),
        });
    }

    Ok(posts)
}

// ---------- handlers ----------

async fn get_initialize(State(state): State<AppState>) -> Response {
    let sqls = [
        "DELETE FROM users WHERE id > 1000",
        "DELETE FROM posts WHERE id > 10000",
        "DELETE FROM comments WHERE id > 100000",
        "UPDATE users SET del_flg = 0",
        "UPDATE users SET del_flg = 1 WHERE id % 50 = 0",
    ];
    for s in sqls {
        let _ = sqlx::query(s).execute(&state.db).await;
    }
    // users table changed (del_flg reset, id>1000 deleted): rebuild the in-memory cache.
    let fresh = load_user_cache(&state.db).await;
    *state.users.write().unwrap() = fresh;
    // remove image files for posts that were just deleted (id > 10000) so nginx never
    // serves a stale image from a previous run for a reused auto_increment id.
    if let Ok(mut rd) = tokio::fs::read_dir(IMAGE_DIR).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some((stem, _ext)) = name.rsplit_once('.') {
                if let Ok(id) = stem.trim_start_matches('.').parse::<i64>() {
                    if id > 10000 {
                        let _ = tokio::fs::remove_file(entry.path()).await;
                    }
                }
            }
        }
    }
    StatusCode::OK.into_response()
}

async fn get_login(State(state): State<AppState>, Extension(sid): Extension<SessionId>) -> Response {
    let me = current_user(&state, &sid.0).await;
    if is_login(&me) {
        return redirect("/");
    }
    let flash = get_flash(&state, &sid.0, "notice");
    render(LoginTemplate { me, flash })
}

#[derive(Deserialize)]
struct LoginForm {
    account_name: String,
    password: String,
}

async fn post_login(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppErr> {
    if is_login(&current_user(&state, &sid.0).await) {
        return Ok(redirect("/"));
    }

    let u: Option<User> = cache_user_by_name(&state, &form.account_name)
        .filter(|u| u.del_flg == 0);

    let matched = match &u {
        Some(u) if calculate_passhash(&u.account_name, &form.password) == u.passhash => Some(u),
        _ => None,
    };

    if let Some(u) = matched {
        let uid = u.id;
        session_set(&state, &sid.0, |s| {
            s.user_id = Some(uid);
            s.csrf_token = Some(rand_hex(16));
        });
        Ok(redirect("/"))
    } else {
        session_set(&state, &sid.0, |s| {
            s.notice = Some("アカウント名かパスワードが間違っています".to_string());
        });
        Ok(redirect("/login"))
    }
}

async fn get_register(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
) -> Response {
    if is_login(&current_user(&state, &sid.0).await) {
        return redirect("/");
    }
    let flash = get_flash(&state, &sid.0, "notice");
    render(RegisterTemplate {
        me: User::empty(),
        flash,
    })
}

async fn post_register(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppErr> {
    if is_login(&current_user(&state, &sid.0).await) {
        return Ok(redirect("/"));
    }

    if !validate_user(&form.account_name, &form.password) {
        session_set(&state, &sid.0, |s| {
            s.notice =
                Some("アカウント名は3文字以上、パスワードは6文字以上である必要があります".to_string());
        });
        return Ok(redirect("/register"));
    }

    if cache_user_by_name(&state, &form.account_name).is_some() {
        session_set(&state, &sid.0, |s| {
            s.notice = Some("アカウント名がすでに使われています".to_string());
        });
        return Ok(redirect("/register"));
    }

    let passhash = calculate_passhash(&form.account_name, &form.password);
    let result = sqlx::query("INSERT INTO `users` (`account_name`, `passhash`) VALUES (?,?)")
        .bind(&form.account_name)
        .bind(&passhash)
        .execute(&state.db)
        .await?;
    let uid = result.last_insert_id() as i32;
    // add the new user to the in-memory cache so subsequent requests see it without a DB hit
    cache_insert_user(
        &state,
        User {
            id: uid,
            account_name: form.account_name.clone(),
            passhash,
            authority: 0,
            del_flg: 0,
            created_at: Utc::now(),
        },
    );
    session_set(&state, &sid.0, |s| {
        s.user_id = Some(uid);
        s.csrf_token = Some(rand_hex(16));
    });
    Ok(redirect("/"))
}

async fn get_logout(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
) -> Response {
    state.sessions.lock().unwrap().remove(&sid.0);
    let mut resp = redirect("/");
    resp.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("session=; Path=/; Max-Age=0; HttpOnly"),
    );
    resp
}

async fn get_index(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
) -> Result<Response, AppErr> {
    let me = current_user(&state, &sid.0).await;
    let rows: Vec<PostRow> = sqlx::query_as(&format!(
        "SELECT `id`, `user_id`, `body`, `mime`, `created_at` FROM `posts` ORDER BY `created_at` DESC LIMIT {}",
        POSTS_FETCH_LIMIT
    ))
    .fetch_all(&state.db)
    .await?;

    let csrf = csrf_token(&state, &sid.0);
    let posts = make_posts(&state, rows, &csrf, false).await?;
    let flash = get_flash(&state, &sid.0, "notice");

    Ok(render(IndexTemplate {
        me,
        posts,
        csrf_token: csrf,
        flash,
    }))
}

async fn get_account_name_handler(
    state: &AppState,
    sid: &str,
    account_name: &str,
) -> Result<Response, AppErr> {
    let user = match cache_user_by_name(state, account_name).filter(|u| u.del_flg == 0) {
        Some(u) => u,
        None => return Ok(StatusCode::NOT_FOUND.into_response()),
    };

    let rows: Vec<PostRow> = sqlx::query_as(
        "SELECT `id`, `user_id`, `body`, `mime`, `created_at` FROM `posts` WHERE `user_id` = ? ORDER BY `created_at` DESC LIMIT 20",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;

    let csrf = csrf_token(state, sid);
    let posts = make_posts(state, rows, &csrf, false).await?;

    let comment_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) AS count FROM `comments` WHERE `user_id` = ?")
            .bind(user.id)
            .fetch_one(&state.db)
            .await?;

    let post_ids: Vec<i32> = sqlx::query_scalar("SELECT `id` FROM `posts` WHERE `user_id` = ?")
        .bind(user.id)
        .fetch_all(&state.db)
        .await?;
    let post_count = post_ids.len() as i64;

    let mut commented_count: i64 = 0;
    if post_count > 0 {
        let placeholder = vec!["?"; post_ids.len()].join(", ");
        let q = format!(
            "SELECT COUNT(*) AS count FROM `comments` WHERE `post_id` IN ({})",
            placeholder
        );
        let mut query = sqlx::query_scalar::<_, i64>(&q);
        for id in &post_ids {
            query = query.bind(id);
        }
        commented_count = query.fetch_one(&state.db).await?;
    }

    let me = current_user(state, sid).await;

    Ok(render(UserTemplate {
        me,
        user,
        posts,
        post_count,
        comment_count,
        commented_count,
    }))
}

async fn get_posts(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppErr> {
    let max_created_at = match params.get("max_created_at") {
        Some(v) if !v.is_empty() => v.clone(),
        _ => return Ok(StatusCode::OK.into_response()),
    };

    let parsed = match chrono::DateTime::parse_from_rfc3339(&max_created_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(e) => {
            eprintln!("max_created_at parse error: {e}");
            return Ok(StatusCode::OK.into_response());
        }
    };

    let rows: Vec<PostRow> = sqlx::query_as(&format!(
        "SELECT `id`, `user_id`, `body`, `mime`, `created_at` FROM `posts` WHERE `created_at` <= ? ORDER BY `created_at` DESC LIMIT {}",
        POSTS_FETCH_LIMIT
    ))
    .bind(parsed)
    .fetch_all(&state.db)
    .await?;

    let csrf = csrf_token(&state, &sid.0);
    let posts = make_posts(&state, rows, &csrf, false).await?;

    if posts.is_empty() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    Ok(render(PostsTemplate { posts }))
}

async fn get_posts_id(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
    Path(id): Path<i32>,
) -> Result<Response, AppErr> {
    let rows: Vec<PostRow> = sqlx::query_as(
        "SELECT `id`, `user_id`, `body`, `mime`, `created_at` FROM `posts` WHERE `id` = ?",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let csrf = csrf_token(&state, &sid.0);
    let mut posts = make_posts(&state, rows, &csrf, true).await?;

    if posts.is_empty() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let post = posts.remove(0);
    let me = current_user(&state, &sid.0).await;
    Ok(render(PostIdTemplate { me, post }))
}

async fn post_index(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
    mut multipart: Multipart,
) -> Result<Response, AppErr> {
    let me = current_user(&state, &sid.0).await;
    if !is_login(&me) {
        return Ok(redirect("/login"));
    }

    let mut file_bytes: Option<Vec<u8>> = None;
    let mut content_type = String::new();
    let mut body = String::new();
    let mut csrf = String::new();

    while let Some(field) = multipart.next_field().await? {
        match field.name().unwrap_or("") {
            "file" => {
                content_type = field
                    .content_type()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let data = field.bytes().await?;
                file_bytes = Some(data.to_vec());
            }
            "body" => {
                body = field.text().await?;
            }
            "csrf_token" => {
                csrf = field.text().await?;
            }
            _ => {
                let _ = field.bytes().await?;
            }
        }
    }

    if csrf != csrf_token(&state, &sid.0) {
        return Ok(StatusCode::UNPROCESSABLE_ENTITY.into_response());
    }

    let filedata = match file_bytes {
        Some(b) if !b.is_empty() => b,
        _ => {
            session_set(&state, &sid.0, |s| {
                s.notice = Some("画像が必須です".to_string());
            });
            return Ok(redirect("/"));
        }
    };

    let mime = if content_type.contains("jpeg") {
        "image/jpeg"
    } else if content_type.contains("png") {
        "image/png"
    } else if content_type.contains("gif") {
        "image/gif"
    } else {
        session_set(&state, &sid.0, |s| {
            s.notice = Some("投稿できる画像形式はjpgとpngとgifだけです".to_string());
        });
        return Ok(redirect("/"));
    };

    if filedata.len() > UPLOAD_LIMIT {
        session_set(&state, &sid.0, |s| {
            s.notice = Some("ファイルサイズが大きすぎます".to_string());
        });
        return Ok(redirect("/"));
    }

    let result = sqlx::query("INSERT INTO `posts` (`user_id`, `mime`, `body`) VALUES (?,?,?)")
        .bind(me.id)
        .bind(mime)
        .bind(&body)
        .execute(&state.db)
        .await?;

    let pid = result.last_insert_id();
    // keep image bytes in the separate `images` table as a fallback for get_image
    sqlx::query("INSERT INTO `images` (`post_id`, `imgdata`) VALUES (?,?)")
        .bind(pid)
        .bind(&filedata)
        .execute(&state.db)
        .await?;
    // write the uploaded image to disk for nginx to serve directly
    write_image_file(pid as i64, mime, &filedata).await;
    Ok(redirect(&format!("/posts/{}", pid)))
}

async fn get_image(
    State(state): State<AppState>,
    Path(filename): Path<String>,
) -> Result<Response, AppErr> {
    let (id_str, ext) = match filename.rsplit_once('.') {
        Some(v) => v,
        None => return Ok(StatusCode::NOT_FOUND.into_response()),
    };
    let pid: i32 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => return Ok(StatusCode::NOT_FOUND.into_response()),
    };

    let row: Option<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT p.`mime`, i.`imgdata` FROM `posts` p JOIN `images` i ON i.`post_id` = p.`id` WHERE p.`id` = ?",
    )
    .bind(pid)
    .fetch_optional(&state.db)
    .await?;

    let (mime, imgdata) = match row {
        Some(v) => v,
        None => return Ok(StatusCode::NOT_FOUND.into_response()),
    };

    let ok = (ext == "jpg" && mime == "image/jpeg")
        || (ext == "png" && mime == "image/png")
        || (ext == "gif" && mime == "image/gif");

    if ok {
        // dump to disk so subsequent requests are served by nginx directly
        write_image_file(pid as i64, &mime, &imgdata).await;
        Ok((
            [(header::CONTENT_TYPE, HeaderValue::from_str(&mime).unwrap())],
            imgdata,
        )
            .into_response())
    } else {
        Ok(StatusCode::NOT_FOUND.into_response())
    }
}

#[derive(Deserialize)]
struct CommentForm {
    comment: String,
    post_id: String,
    csrf_token: String,
}

async fn post_comment(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
    Form(form): Form<CommentForm>,
) -> Result<Response, AppErr> {
    let me = current_user(&state, &sid.0).await;
    if !is_login(&me) {
        return Ok(redirect("/login"));
    }
    if form.csrf_token != csrf_token(&state, &sid.0) {
        return Ok(StatusCode::UNPROCESSABLE_ENTITY.into_response());
    }
    let post_id: i32 = match form.post_id.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("post_idは整数のみです");
            return Ok(StatusCode::OK.into_response());
        }
    };

    sqlx::query("INSERT INTO `comments` (`post_id`, `user_id`, `comment`) VALUES (?,?,?)")
        .bind(post_id)
        .bind(me.id)
        .bind(&form.comment)
        .execute(&state.db)
        .await?;

    Ok(redirect(&format!("/posts/{}", post_id)))
}

async fn get_admin_banned(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
) -> Result<Response, AppErr> {
    let me = current_user(&state, &sid.0).await;
    if !is_login(&me) {
        return Ok(redirect("/"));
    }
    if me.authority == 0 {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let users: Vec<User> = sqlx::query_as(
        "SELECT * FROM `users` WHERE `authority` = 0 AND `del_flg` = 0 ORDER BY `created_at` DESC",
    )
    .fetch_all(&state.db)
    .await?;

    let csrf = csrf_token(&state, &sid.0);
    Ok(render(BannedTemplate {
        me,
        users,
        csrf_token: csrf,
    }))
}

async fn post_admin_banned(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
    body: Bytes,
) -> Result<Response, AppErr> {
    let me = current_user(&state, &sid.0).await;
    if !is_login(&me) {
        return Ok(redirect("/"));
    }
    if me.authority == 0 {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let mut csrf = String::new();
    let mut uids: Vec<String> = Vec::new();
    for (k, v) in form_urlencoded::parse(&body) {
        match k.as_ref() {
            "csrf_token" => csrf = v.into_owned(),
            "uid[]" => uids.push(v.into_owned()),
            _ => {}
        }
    }

    if csrf != csrf_token(&state, &sid.0) {
        return Ok(StatusCode::UNPROCESSABLE_ENTITY.into_response());
    }

    for id in uids {
        let _ = sqlx::query("UPDATE `users` SET `del_flg` = ? WHERE `id` = ?")
            .bind(1)
            .bind(&id)
            .execute(&state.db)
            .await;
        if let Ok(uid) = id.parse::<i32>() {
            cache_set_del_flg(&state, uid, 1);
        }
    }

    Ok(redirect("/admin/banned"))
}

// fallback: handle "/@account" and static files
async fn fallback(
    State(state): State<AppState>,
    Extension(sid): Extension<SessionId>,
    req: Request,
) -> Response {
    let path = req.uri().path().to_string();
    if let Some(rest) = path.strip_prefix("/@") {
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return match get_account_name_handler(&state, &sid.0, rest).await {
                Ok(resp) => resp,
                Err(e) => e.into_response(),
            };
        }
    }
    // static files from public/
    let serve = ServeDir::new(PUBLIC_DIR);
    match serve.oneshot(req).await {
        Ok(resp) => resp.into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------- session middleware ----------

async fn session_layer(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let existing = cookie_value(&req, "session");
    let (sid, is_new) = match existing {
        Some(s) if state.sessions.lock().unwrap().contains_key(&s) => (s, false),
        _ => (rand_hex(16), true),
    };
    state
        .sessions
        .lock()
        .unwrap()
        .entry(sid.clone())
        .or_default();
    req.extensions_mut().insert(SessionId(sid.clone()));

    let mut resp = next.run(req).await;
    if is_new {
        resp.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!("session={}; Path=/; HttpOnly", sid)).unwrap(),
        );
    }
    resp
}

// ---------- main ----------

#[tokio::main]
async fn main() {
    let host = std::env::var("ISUCONP_DB_HOST").unwrap_or_else(|_| "localhost".to_string());
    let port: u16 = std::env::var("ISUCONP_DB_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3306);
    let user = std::env::var("ISUCONP_DB_USER").unwrap_or_else(|_| "root".to_string());
    let password = std::env::var("ISUCONP_DB_PASSWORD").unwrap_or_default();
    let dbname = std::env::var("ISUCONP_DB_NAME").unwrap_or_else(|_| "isuconp".to_string());

    let connect_opts = MySqlConnectOptions::new()
        .host(&host)
        .port(port)
        .username(&user)
        .password(&password)
        .database(&dbname)
        .charset("utf8mb4");

    let db = MySqlPoolOptions::new()
        .max_connections(30)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute("SET time_zone = '+00:00'").await?;
                Ok(())
            })
        })
        .connect_with(connect_opts)
        .await
        .expect("failed to connect to DB");

    let user_cache = load_user_cache(&db).await;
    let state = AppState {
        db,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        users: Arc::new(RwLock::new(user_cache)),
    };

    let app = Router::new()
        .route("/initialize", get(get_initialize))
        .route("/login", get(get_login).post(post_login))
        .route("/register", get(get_register).post(post_register))
        .route("/logout", get(get_logout))
        .route("/", get(get_index).post(post_index))
        .route("/posts", get(get_posts))
        .route("/posts/:id", get(get_posts_id))
        .route("/image/:filename", get(get_image))
        .route("/comment", post(post_comment))
        .route(
            "/admin/banned",
            get(get_admin_banned).post(post_admin_banned),
        )
        .fallback(fallback)
        .layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), session_layer))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080")
        .await
        .expect("failed to bind");
    println!("listening on 127.0.0.1:8080");
    axum::serve(listener, app).await.unwrap();
}
