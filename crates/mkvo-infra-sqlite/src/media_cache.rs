use super::*;

/// Matches `?1` and everything below it by primary-key range: case-sensitive
/// and indexed, unlike `LIKE`. `0` and `]` are the characters after `/` and `\`.
const UNDER_ROOT: &str = "(file_path = ?1
    OR (file_path >= ?1 || '/' AND file_path < ?1 || '0')
    OR (file_path >= ?1 || char(92) AND file_path < ?1 || ']'))";

fn subtree_root(root: &Path) -> String {
    path_text(root).trim_end_matches(['/', '\\']).to_owned()
}

impl SqliteStore {
    pub fn cache_count(&self) -> StoreResult<u64> {
        let connection = self.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM media_cache", [], |row| row.get(0))?;
        u64::try_from(count).map_err(|_| StoreError::NumericOverflow)
    }

    pub fn list_media_under(&self, root: &Path) -> StoreResult<Vec<CachedMedia>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT file_path, file_size, modified_at_ns, quick_hash, tool_fingerprint,
                    scanned_at_ms, payload_json
             FROM media_cache
             WHERE {UNDER_ROOT}
             ORDER BY file_path"
        ))?;
        let rows = statement.query_map([subtree_root(root)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (path, size, modified_at_ns, quick_hash, tool_fingerprint, scanned_at_ms, payload) =
                row?;
            Ok(CachedMedia {
                fingerprint: CacheFingerprint {
                    path: PathBuf::from(path),
                    file_size: u64::try_from(size).map_err(|_| StoreError::NumericOverflow)?,
                    modified_at_ns,
                    quick_hash,
                    tool_fingerprint,
                },
                scanned_at_ms,
                payload: serde_json::from_str(&payload)?,
            })
        })
        .collect()
    }

    pub fn get_valid_media(
        &self,
        fingerprint: &CacheFingerprint,
    ) -> StoreResult<Option<CachedMedia>> {
        let path = path_text(&fingerprint.path);
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT file_size, modified_at_ns, quick_hash, tool_fingerprint, scanned_at_ms, payload_json
                 FROM media_cache WHERE file_path = ?1",
                [&path],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((size, modified_at_ns, quick_hash, tool_fingerprint, scanned_at_ms, payload)) =
            row
        else {
            return Ok(None);
        };
        let size_matches = u64::try_from(size).ok() == Some(fingerprint.file_size);
        let quick_hash_matches = fingerprint.quick_hash.is_none()
            || quick_hash.is_none()
            || quick_hash == fingerprint.quick_hash;
        let matches = size_matches
            && modified_at_ns == fingerprint.modified_at_ns
            && quick_hash_matches
            && tool_fingerprint == fingerprint.tool_fingerprint;
        if !matches {
            connection.execute("DELETE FROM media_cache WHERE file_path = ?1", [&path])?;
            return Ok(None);
        }
        Ok(Some(CachedMedia {
            fingerprint: fingerprint.clone(),
            scanned_at_ms,
            payload: serde_json::from_str(&payload)?,
        }))
    }

    pub fn upsert_media(&self, media: &CachedMedia) -> StoreResult<()> {
        let size =
            i64::try_from(media.fingerprint.file_size).map_err(|_| StoreError::NumericOverflow)?;
        let payload = serde_json::to_string(&media.payload)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO media_cache
             (file_path, file_size, modified_at_ns, quick_hash, tool_fingerprint, scanned_at_ms, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(file_path) DO UPDATE SET
               file_size=excluded.file_size,
               modified_at_ns=excluded.modified_at_ns,
               quick_hash=excluded.quick_hash,
               tool_fingerprint=excluded.tool_fingerprint,
               scanned_at_ms=excluded.scanned_at_ms,
               payload_json=excluded.payload_json",
            params![
                path_text(&media.fingerprint.path),
                size,
                media.fingerprint.modified_at_ns,
                media.fingerprint.quick_hash,
                media.fingerprint.tool_fingerprint,
                media.scanned_at_ms,
                payload,
            ],
        )?;
        Ok(())
    }

    pub fn remove_media(&self, path: &Path) -> StoreResult<bool> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "DELETE FROM media_cache WHERE file_path = ?1",
            [path_text(path)],
        )? > 0)
    }

    pub fn remove_media_under(&self, root: &Path) -> StoreResult<u64> {
        let connection = self.connection()?;
        let removed = connection.execute(
            &format!("DELETE FROM media_cache WHERE {UNDER_ROOT}"),
            [subtree_root(root)],
        )?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }

    pub fn remove_media_older_than(&self, cutoff_ms: i64) -> StoreResult<u64> {
        let connection = self.connection()?;
        let removed = connection.execute(
            "DELETE FROM media_cache WHERE scanned_at_ms < ?1",
            [cutoff_ms],
        )?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }

    pub fn clear_media_cache(&self) -> StoreResult<u64> {
        let connection = self.connection()?;
        let removed = connection.execute("DELETE FROM media_cache", [])?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store_with(paths: &[&str]) -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("open store");
        for path in paths {
            store
                .upsert_media(&CachedMedia {
                    fingerprint: fingerprint(path),
                    scanned_at_ms: 100,
                    payload: json!({"tracks": 1}),
                })
                .expect("upsert");
        }
        store
    }

    fn fingerprint(path: &str) -> CacheFingerprint {
        CacheFingerprint {
            path: PathBuf::from(path),
            file_size: 10,
            modified_at_ns: 123,
            quick_hash: None,
            tool_fingerprint: String::new(),
        }
    }

    fn cached(store: &SqliteStore, path: &str) -> bool {
        store
            .get_valid_media(&fingerprint(path))
            .expect("read")
            .is_some()
    }

    #[test]
    fn subtree_pruning_is_case_sensitive_and_stops_at_the_folder_boundary() {
        let store = store_with(&[
            "/media/Anime/a.mkv",
            "/media/Anime/Show/b.mkv",
            "/media/anime/c.mkv",
            "/media/Anime2/d.mkv",
            "/media/Anime.old/e.mkv",
        ]);

        assert_eq!(
            store
                .remove_media_under(Path::new("/media/Anime/"))
                .expect("remove under"),
            2
        );
        assert!(cached(&store, "/media/anime/c.mkv"));
        assert!(cached(&store, "/media/Anime2/d.mkv"));
        assert!(cached(&store, "/media/Anime.old/e.mkv"));
    }

    #[test]
    fn subtree_match_searches_the_primary_key_instead_of_scanning() {
        let store = store_with(&[]);
        let connection = store.connection().expect("connection");
        let plan: Vec<String> = connection
            .prepare(&format!(
                "EXPLAIN QUERY PLAN DELETE FROM media_cache WHERE {UNDER_ROOT}"
            ))
            .expect("prepare")
            .query_map(["/media"], |row| row.get(3))
            .expect("plan")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert!(
            plan.iter().any(|step| step.starts_with("SEARCH")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.starts_with("SCAN")),
            "{plan:?}"
        );
    }

    #[test]
    fn subtree_listing_is_case_sensitive_and_treats_wildcards_literally() {
        let store = store_with(&[
            "/media/100%_done/a.mkv",
            "/media/100%_DONE/b.mkv",
            "/media/100x-done/c.mkv",
        ]);

        let listed = store
            .list_media_under(Path::new("/media/100%_done"))
            .expect("list under");
        assert_eq!(
            listed
                .into_iter()
                .map(|media| media.fingerprint.path)
                .collect::<Vec<_>>(),
            [PathBuf::from("/media/100%_done/a.mkv")]
        );
    }
}
