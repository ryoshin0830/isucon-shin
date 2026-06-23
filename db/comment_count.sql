-- posts に comment_count を非正規化し、make_posts の COUNT(*) GROUP BY クエリを撲滅する。
-- 一覧/詳細/ユーザページの最ホットパスから 1 クエリ削減（レイテンシ律速の緩和）。
-- 維持: コメント投稿時に +1、/initialize で再計算（アプリ側で実施）。

ALTER TABLE posts ADD COLUMN comment_count INT NOT NULL DEFAULT 0;

UPDATE posts SET comment_count = 0;
UPDATE posts p
  JOIN (SELECT post_id, COUNT(*) c FROM comments GROUP BY post_id) x
    ON p.id = x.post_id
  SET p.comment_count = x.c;
