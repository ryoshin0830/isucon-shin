-- private-isu 追加インデックス
-- makePosts の N+1（各投稿ごとの COUNT/SELECT comments）と一覧の ORDER BY を高速化。
-- comments は 100,000 行・posts は 10,000 行で、初期状態は PK と users.account_name UNIQUE 以外に索引が無く、
-- post_id / user_id / created_at による絞り込み・整列が全表走査になっていた。
-- 注意: /initialize は行の DELETE/UPDATE のみでインデックスは落とさないため、これらは永続する。

ALTER TABLE comments ADD INDEX idx_post_id_created_at (post_id, created_at);
ALTER TABLE comments ADD INDEX idx_user_id (user_id);
ALTER TABLE posts ADD INDEX idx_created_at (created_at);
ALTER TABLE posts ADD INDEX idx_user_id_created_at (user_id, created_at);
