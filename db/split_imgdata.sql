-- posts テーブルから imgdata(BLOB, 計1.3GB) を削除し、別テーブル images へ分離する。
-- 目的: posts のクラスタ化インデックス行を極小化し、
--   `ORDER BY created_at DESC LIMIT 20` + PK ルックアップ + JOIN users を高速化する。
--   （imgdata がインラインだと 20 行返すだけで巨大ページを引きずり 1 クエリ 0.19s かかっていた）
--
-- 既存 10000 枚の画像は public/image/<id>.<ext> にファイル化済みで nginx が直配信するため、
--   imgdata を DB に複製する必要はない（ディスク残量も無い）。images テーブルは空で作成し、
--   以降の新規投稿のみ get_image フォールバック用に格納する。
-- 注意: /initialize は行 DELETE/UPDATE のみでスキーマは戻さないため、この変更は永続する。

CREATE TABLE IF NOT EXISTS images (
  post_id INT NOT NULL PRIMARY KEY,
  imgdata MEDIUMBLOB NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

ALTER TABLE posts DROP COLUMN imgdata;
