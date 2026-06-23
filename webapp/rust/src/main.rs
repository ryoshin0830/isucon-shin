use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
use chrono::NaiveDateTime;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha512};
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::{Executor, MySqlPool};
use tower::ServiceExt;
use tower_http::services::ServeDir;

const POSTS_PER_PAGE: usize = 20;
const UPLOAD_LIMIT: usize = 10 * 1024 * 1024;
const PUBLIC_DIR: &str = "/home/isucon/private_isu/webapp/public";

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
    created_at: NaiveDateTime,
}

impl User {
    fn empty() -> Self {
        User {
            id: 0,
            account_name: String::new(),
            passhash: String::new(),
            authority: 0,
            del_flg: 0,
            created_at: NaiveDateTime::UNIX_EPOCH,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostRow {
    id: i32,
    user_id: i32,
    body: String,
    mime: String,
    created_at: NaiveDateTime,
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
    created_at: NaiveDateTime,
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
    created_at: NaiveDateTime,
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
        format!("{}+09:00", self.created_at.format("%Y-%m-%dT%H:%M:%S"))
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

#[derive(Clone)]
struct AppState {
    db: MySqlPool,
    sessions: SessionStore,
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
        match sqlx::query_as::<_, User>("SELECT * FROM `users` WHERE `id` = ?")
            .bind(uid)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(u)) => u,
            _ => User::empty(),
        }
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

async fn make_posts(
    state: &AppState,
    rows: Vec<PostRow>,
    csrf: &str,
    all_comments: bool,
) -> Result<Vec<Post>, sqlx::Error> {
    let mut posts: Vec<Post> = Vec::new();

    for r in rows {
        let comment_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) AS `count` FROM `comments` WHERE `post_id` = ?")
                .bind(r.id)
                .fetch_one(&state.db)
                .await?;

        let query = if all_comments {
            "SELECT * FROM `comments` WHERE `post_id` = ? ORDER BY `created_at` DESC"
        } else {
            "SELECT * FROM `comments` WHERE `post_id` = ? ORDER BY `created_at` DESC LIMIT 3"
        };
        let crows: Vec<CommentRow> = sqlx::query_as(query).bind(r.id).fetch_all(&state.db).await?;

        let mut comments: Vec<Comment> = Vec::with_capacity(crows.len());
        for c in crows {
            let u: User = sqlx::query_as("SELECT * FROM `users` WHERE `id` = ?")
                .bind(c.user_id)
                .fetch_one(&state.db)
                .await?;
            comments.push(Comment {
                comment: c.comment,
                user: u,
            });
        }
        comments.reverse();

        let puser: User = sqlx::query_as("SELECT * FROM `users` WHERE `id` = ?")
            .bind(r.user_id)
            .fetch_one(&state.db)
            .await?;

        if puser.del_flg == 0 {
            posts.push(Post {
                id: r.id,
                user: puser,
                body: r.body,
                mime: r.mime,
                created_at: r.created_at,
                comment_count,
                comments,
                csrf_token: csrf.to_string(),
            });
        }
        if posts.len() >= POSTS_PER_PAGE {
            break;
        }
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

    let u: Option<User> =
        sqlx::query_as("SELECT * FROM users WHERE account_name = ? AND del_flg = 0")
            .bind(&form.account_name)
            .fetch_optional(&state.db)
            .await?;

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

    let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM users WHERE `account_name` = ?")
        .bind(&form.account_name)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_some() {
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
    let rows: Vec<PostRow> = sqlx::query_as(
        "SELECT `id`, `user_id`, `body`, `mime`, `created_at` FROM `posts` ORDER BY `created_at` DESC",
    )
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
    let user: Option<User> =
        sqlx::query_as("SELECT * FROM `users` WHERE `account_name` = ? AND `del_flg` = 0")
            .bind(account_name)
            .fetch_optional(&state.db)
            .await?;
    let user = match user {
        Some(u) => u,
        None => return Ok(StatusCode::NOT_FOUND.into_response()),
    };

    let rows: Vec<PostRow> = sqlx::query_as(
        "SELECT `id`, `user_id`, `body`, `mime`, `created_at` FROM `posts` WHERE `user_id` = ? ORDER BY `created_at` DESC",
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
        Ok(dt) => dt.naive_local(),
        Err(e) => {
            eprintln!("max_created_at parse error: {e}");
            return Ok(StatusCode::OK.into_response());
        }
    };

    let rows: Vec<PostRow> = sqlx::query_as(
        "SELECT `id`, `user_id`, `body`, `mime`, `created_at` FROM `posts` WHERE `created_at` <= ? ORDER BY `created_at` DESC",
    )
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

    let result = sqlx::query(
        "INSERT INTO `posts` (`user_id`, `mime`, `imgdata`, `body`) VALUES (?,?,?,?)",
    )
    .bind(me.id)
    .bind(mime)
    .bind(&filedata)
    .bind(&body)
    .execute(&state.db)
    .await?;

    let pid = result.last_insert_id();
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

    let row: Option<(String, Vec<u8>)> =
        sqlx::query_as("SELECT `mime`, `imgdata` FROM `posts` WHERE `id` = ?")
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
                conn.execute("SET time_zone = '+09:00'").await?;
                Ok(())
            })
        })
        .connect_with(connect_opts)
        .await
        .expect("failed to connect to DB");

    let state = AppState {
        db,
        sessions: Arc::new(Mutex::new(HashMap::new())),
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
