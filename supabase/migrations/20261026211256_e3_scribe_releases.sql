INSERT INTO storage.buckets (id, name, public, file_size_limit)
VALUES ('e3-scribe-releases', 'e3-scribe-releases', true, 1073741824)
ON CONFLICT (id) DO UPDATE SET public = true, file_size_limit = EXCLUDED.file_size_limit;
