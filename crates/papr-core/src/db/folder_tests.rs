use super::*;

fn database() -> Connection {
    open(Path::new(":memory:")).unwrap()
}
fn subscribe(conn: &Connection, folder: i64, name: &str) -> i64 {
    conn.execute(
        "INSERT INTO feeds(feed_url,title,folder_id) VALUES(?1,?2,?3)",
        params![format!("https://{name}.example/rss"), name, folder],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO articles(feed_id,guid,title) VALUES(?1,'one','Article')",
        [id],
    )
    .unwrap();
    id
}
#[test]
fn parent_views_refresh_and_bulk_read_include_descendants() {
    let conn = database();
    let root = create_folder(&conn, "Root").unwrap();
    let child = create_folder_in(&conn, "Child", Some(root)).unwrap();
    let leaf = create_folder_in(&conn, "Leaf", Some(child)).unwrap();
    let other = create_folder(&conn, "Other").unwrap();
    for (folder, name) in [
        (root, "root"),
        (child, "child"),
        (leaf, "leaf"),
        (other, "other"),
    ] {
        subscribe(&conn, folder, name);
    }
    assert_eq!(
        list_articles(
            &conn,
            &ArticleQuery::Folder(root),
            false,
            None,
            false,
            50,
            0
        )
        .unwrap()
        .len(),
        3
    );
    assert_eq!(feeds_to_refresh_in_folder(&conn, root).unwrap().len(), 3);
    assert_eq!(
        mark_all_read(&conn, &ArticleQuery::Folder(root), false).unwrap(),
        3
    );
    assert_eq!(count_unread(&conn).unwrap(), 1);
}
#[test]
fn folder_moves_reject_cycles_and_missing_parents() {
    let conn = database();
    let root = create_folder(&conn, "Root").unwrap();
    let child = create_folder_in(&conn, "Child", Some(root)).unwrap();
    assert!(matches!(
        move_folder(&conn, root, Some(child)),
        Err(AppError::Coded("folderCycle"))
    ));
    assert!(move_folder(&conn, child, Some(child)).is_err());
    assert!(move_folder(&conn, child, Some(9999)).is_err());
    assert!(conn
        .execute(
            "UPDATE folders SET parent_id=?2 WHERE id=?1",
            params![root, child]
        )
        .is_err());
    move_folder(&conn, child, None).unwrap();
    assert!(list_folders(&conn)
        .unwrap()
        .iter()
        .all(|folder| folder.parent_id.is_none()));
}
#[test]
fn deleting_only_parent_preserves_children_and_subscriptions() {
    let conn = database();
    let root = create_folder(&conn, "Root").unwrap();
    let child = create_folder_in(&conn, "Child", Some(root)).unwrap();
    let root_feed = subscribe(&conn, root, "root");
    let child_feed = subscribe(&conn, child, "child");
    delete_folder(&conn, root).unwrap();
    assert_eq!(list_folders(&conn).unwrap()[0].parent_id, None);
    assert_eq!(feed_folder_id(&conn, root_feed).unwrap(), None);
    assert_eq!(feed_folder_id(&conn, child_feed).unwrap(), Some(child));
    assert_eq!(list_feeds(&conn).unwrap().len(), 2);
}
#[test]
fn deleting_subtree_removes_only_its_feeds_and_articles_and_rolls_back_on_failure() {
    let conn = database();
    let root = create_folder(&conn, "Root").unwrap();
    let child = create_folder_in(&conn, "Child", Some(root)).unwrap();
    let other = create_folder(&conn, "Other").unwrap();
    subscribe(&conn, root, "root");
    subscribe(&conn, child, "child");
    subscribe(&conn, other, "other");
    conn.execute_batch("CREATE TRIGGER fail_folder_delete BEFORE DELETE ON folders WHEN old.name='Child' BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    assert!(delete_folder_with_feeds(&conn, root).is_err());
    assert_eq!(list_feeds(&conn).unwrap().len(), 3);
    conn.execute_batch("DROP TRIGGER fail_folder_delete;")
        .unwrap();
    delete_folder_with_feeds(&conn, root).unwrap();
    assert_eq!(list_folders(&conn).unwrap().len(), 1);
    assert_eq!(list_feeds(&conn).unwrap()[0].folder_id, Some(other));
    assert_eq!(count_unread(&conn).unwrap(), 1);
}
#[test]
fn hierarchy_rename_and_subtree_deletion_sync_across_devices() {
    let a = database();
    let mut b = database();
    let root = create_folder(&a, "Z parent").unwrap();
    let child = create_folder_in(&a, "A child", Some(root)).unwrap();
    subscribe(&a, child, "child");
    webdav::merge(&mut b, &webdav::records(&a).unwrap()).unwrap();
    let folders = list_folders(&b).unwrap();
    let parent = folders.iter().find(|f| f.name == "Z parent").unwrap().id;
    assert_eq!(
        folders
            .iter()
            .find(|f| f.name == "A child")
            .unwrap()
            .parent_id,
        Some(parent)
    );
    rename_folder(&a, root, "Renamed").unwrap();
    webdav::merge(&mut b, &webdav::records(&a).unwrap()).unwrap();
    let folders = list_folders(&b).unwrap();
    let parent = folders.iter().find(|f| f.name == "Renamed").unwrap().id;
    assert_eq!(
        folders
            .iter()
            .find(|f| f.name == "A child")
            .unwrap()
            .parent_id,
        Some(parent)
    );
    delete_folder_with_feeds(&a, root).unwrap();
    webdav::merge(&mut b, &webdav::records(&a).unwrap()).unwrap();
    assert!(list_folders(&b).unwrap().is_empty());
    assert!(list_feeds(&b).unwrap().is_empty());
    assert_eq!(count_unread(&b).unwrap(), 0);
}
#[test]
fn nested_opml_round_trips_without_losing_parent_names() {
    let conn = database();
    let root = create_folder(&conn, "Parent / literal slash").unwrap();
    let child = create_folder_in(&conn, "Child", Some(root)).unwrap();
    subscribe(&conn, child, "child");
    let xml = crate::opml::build_with_folders(
        &feeds_for_export(&conn).unwrap(),
        &list_folders(&conn).unwrap(),
    )
    .unwrap();
    let feeds = crate::opml::parse(&xml).unwrap();
    assert_eq!(
        feeds[0].folder_path,
        vec!["Parent / literal slash", "Child"]
    );
    let imported = database();
    let id = folder_id_by_path(&imported, &feeds[0].folder_path)
        .unwrap()
        .unwrap();
    assert!(list_folders(&imported)
        .unwrap()
        .iter()
        .find(|f| f.id == id)
        .unwrap()
        .parent_id
        .is_some());
}

#[test]
fn identical_child_names_are_scoped_and_sync_memberships_after_moves() {
    let a = database();
    let mut b = database();
    let x = create_folder(&a, "AI").unwrap();
    let y = create_folder(&a, "News").unwrap();
    let xc = create_folder_in(&a, "公众号", Some(x)).unwrap();
    let yc = create_folder_in(&a, "公众号", Some(y)).unwrap();
    assert_ne!(xc, yc);
    assert_eq!(create_folder_in(&a, "公众号", Some(y)).unwrap(), yc);
    subscribe(&a, xc, "ai");
    subscribe(&a, yc, "news");
    webdav::merge_remote(&mut b, &webdav::records(&a).unwrap()).unwrap();
    let paths = |c: &Connection| -> Vec<(String, String)> {
        let mut s=c.prepare("SELECT f.title,p.name FROM feeds f JOIN folders leaf ON leaf.id=f.folder_id JOIN folders p ON p.id=leaf.parent_id ORDER BY f.title").unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(paths(&a), paths(&b));
    assert_eq!(
        paths(&b),
        vec![("ai".into(), "AI".into()), ("news".into(), "News".into())]
    );
    assert!(matches!(
        move_folder(&a, xc, Some(y)),
        Err(AppError::Coded("folderNameExists"))
    ));
    rename_folder(&a, x, "AI renamed").unwrap();
    webdav::merge_remote(&mut b, &webdav::records(&a).unwrap()).unwrap();
    assert_eq!(paths(&a), paths(&b));
    let z = create_folder(&a, "Third").unwrap();
    move_folder(&a, xc, Some(z)).unwrap();
    webdav::merge_remote(&mut b, &webdav::records(&a).unwrap()).unwrap();
    assert_eq!(paths(&a), paths(&b));
    delete_folder(&a, y).unwrap();
    webdav::merge_remote(&mut b, &webdav::records(&a).unwrap()).unwrap();
    assert_eq!(list_feeds(&a).unwrap().len(), 2);
    assert_eq!(list_feeds(&b).unwrap().len(), 2);
}

#[test]
fn v2_migration_retries_previously_skipped_paths_and_nested_paths() {
    let mut c = Connection::open_in_memory().unwrap();
    MIGRATIONS.to_version(&mut c, 27).unwrap();
    set_setting(&c, "folder_path_upgrade_v1", "done").unwrap();
    c.execute("INSERT INTO folders(id,name,parent_id) VALUES(1,'AI',NULL),(2,'公众号',1),(3,'News/公众号',NULL),(4,'Research',NULL),(5,'Reports/公众号',4)",[]).unwrap();
    let a = subscribe(&c, 2, "ai");
    let b = subscribe(&c, 3, "news");
    let d = subscribe(&c, 5, "research");
    set_read(&c, a, true).unwrap();
    set_starred(&c, b, true).unwrap();
    webdav::set_progress(&c, d, 0.8).unwrap();
    MIGRATIONS.to_latest(&mut c).unwrap();
    webdav::upgrade_folder_scope(&mut c).unwrap();
    upgrade_legacy_folder_paths(&mut c).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM folders WHERE instr(name,'/')>0",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM folders WHERE name='公众号'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    assert_eq!(feed_folder_id(&c, b).unwrap(), Some(3));
    assert_eq!(feed_folder_id(&c, d).unwrap(), Some(5));
    assert!(get_article(&c, a).unwrap().is_read);
    assert!(get_article(&c, b).unwrap().is_starred);
    assert_eq!(get_article(&c, d).unwrap().reading_progress, 0.8);
    let mut other = database();
    webdav::merge_remote(&mut other, &webdav::records(&c).unwrap()).unwrap();
    assert_eq!(list_feeds(&other).unwrap().len(), 3);
    let before = webdav::records(&c).unwrap();
    upgrade_legacy_folder_paths(&mut c).unwrap();
    assert_eq!(before, webdav::records(&c).unwrap());
}
#[test]
fn opml_keeps_same_named_children_in_separate_branches() {
    let c = database();
    let a = create_folder(&c, "AI").unwrap();
    let n = create_folder(&c, "News").unwrap();
    for (parent, feed) in [(a, "ai"), (n, "news")] {
        let leaf = create_folder_in(&c, "公众号", Some(parent)).unwrap();
        subscribe(&c, leaf, feed);
    }
    let xml = export_opml(&c).unwrap();
    let feeds = crate::opml::parse(&xml).unwrap();
    assert_eq!(feeds[0].folder_path, vec!["AI", "公众号"]);
    assert_eq!(feeds[1].folder_path, vec!["News", "公众号"]);
    let imported = database();
    for feed in feeds {
        let folder = folder_id_by_path(&imported, &feed.folder_path)
            .unwrap()
            .unwrap();
        subscribe(&imported, folder, &feed.title);
    }
    assert_eq!(
        list_folders(&imported)
            .unwrap()
            .iter()
            .filter(|f| f.name == "公众号")
            .count(),
        2
    );
}
#[test]
fn upgrade_preserves_a_pending_legacy_folder_move_against_old_remote_snapshot() {
    let mut c = Connection::open_in_memory().unwrap();
    MIGRATIONS.to_version(&mut c, 27).unwrap();
    let a = create_folder(&c, "A").unwrap();
    let b = create_folder(&c, "B").unwrap();
    let child = create_folder_in(&c, "Child", Some(a)).unwrap();
    let feed = subscribe(&c, child, "moved");
    let mut remote = webdav::records(&c).unwrap();
    webdav::acknowledge(&c, &remote).unwrap();
    move_folder(&c, child, Some(b)).unwrap();
    MIGRATIONS.to_latest(&mut c).unwrap();
    webdav::upgrade_folder_scope(&mut c).unwrap();
    webdav::upgrade_legacy_folders(&mut remote).unwrap();
    webdav::merge_remote(&mut c, &remote).unwrap();
    assert_eq!(c.query_row("SELECT p.name FROM feeds f JOIN folders l ON l.id=f.folder_id JOIN folders p ON p.id=l.parent_id WHERE f.id=?1",[feed],|r|r.get::<_,String>(0)).unwrap(),"B");
    assert_eq!(
        c.query_row("SELECT count(*) FROM folders WHERE name='Child'", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn per_feed_limit_protects_collections_syncs_deletions_and_compacts_bodies() {
    let c = database();
    let root = create_folder(&c, "Limited").unwrap();
    let feed = subscribe(&c, root, "limit");
    c.execute("DELETE FROM articles", []).unwrap();
    for i in 0..110 {
        c.execute("INSERT INTO articles(feed_id,guid,title,body_text,published_at) VALUES(?1,?2,'Article',?3,datetime('2026-01-01',?4))",params![feed,format!("item-{i}"),"old article content ".repeat(500),format!("+{i} days")]).unwrap();
    }
    set_starred(&c, 1, true).unwrap();
    set_read_later(&c, 2, true).unwrap();
    c.execute(
        "INSERT INTO highlights(article_id,quote,note) VALUES(3,'old','keep')",
        [],
    )
    .unwrap();
    let plan = preview_feed_cleanup(&c, feed).unwrap();
    assert_eq!(plan.remove, 7);
    assert_eq!(plan.total, 110);
    let mut other = database();
    webdav::merge_remote(&mut other, &webdav::records(&c).unwrap()).unwrap();
    assert_eq!(
        cleanup_feed_articles(&c, feed, &plan.token, true).unwrap(),
        7
    );
    assert_eq!(list_feeds(&c).unwrap()[0].max_articles, Some(100));
    assert_eq!(
        c.query_row("SELECT count(*) FROM articles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        103
    );
    assert_eq!(c.query_row("SELECT count(*) FROM webdav_records WHERE kind='article' AND deleted=1 AND json_extract(data,'$.body_text') IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    webdav::merge_remote(&mut other, &webdav::records(&c).unwrap()).unwrap();
    assert_eq!(list_feeds(&other).unwrap()[0].max_articles, Some(100));
    assert_eq!(
        other
            .query_row("SELECT count(*) FROM articles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        103
    );
    c.execute(
        "INSERT INTO articles(feed_id,guid,title,published_at) VALUES(?1,'new','New','2027-01-01')",
        [feed],
    )
    .unwrap();
    assert_eq!(cleanup_limited_feeds(&c).unwrap(), 1);
    disable_feed_limit(&c, feed).unwrap();
    assert_eq!(cleanup_limited_feeds(&c).unwrap(), 0);
}
#[test]
fn cleanup_rejects_stale_preview_and_leaves_protected_content_intact() {
    let c = database();
    let root = create_folder(&c, "Preview").unwrap();
    let feed = subscribe(&c, root, "preview");
    for i in 0..105 {
        c.execute(
            "INSERT INTO articles(feed_id,guid,title) VALUES(?1,?2,'Article')",
            params![feed, format!("{i}")],
        )
        .unwrap();
    }
    let plan = preview_feed_cleanup(&c, feed).unwrap();
    let candidate: i64 = c
        .query_row(
            &format!("SELECT id FROM articles WHERE {FEED_PURGE} LIMIT 1"),
            [feed],
            |r| r.get(0),
        )
        .unwrap();
    set_starred(&c, candidate, true).unwrap();
    assert!(matches!(
        cleanup_feed_articles(&c, feed, &plan.token, true),
        Err(AppError::Coded("feedCleanupChanged"))
    ));
    assert_eq!(list_feeds(&c).unwrap()[0].max_articles, None);
    assert!(get_article(&c, candidate).unwrap().is_starred);
}

#[test]
fn offline_annotations_survive_another_devices_cleanup_and_restore_on_both_devices() {
    let mut a = database();
    let mut b = database();
    let folder = create_folder(&a, "Offline").unwrap();
    let feed = subscribe(&a, folder, "offline");
    for i in 0..105 {
        a.execute("INSERT INTO articles(feed_id,guid,title,body_text,published_at) VALUES(?1,?2,'Article','preserved body',datetime('2026-01-01',?3))",params![feed,format!("item-{i}"),format!("+{i} days")]).unwrap();
    }
    let original = webdav::records(&a).unwrap();
    webdav::merge_remote(&mut b, &original).unwrap();
    webdav::acknowledge(&a, &original).unwrap();
    let target: i64 = b
        .query_row("SELECT id FROM articles WHERE guid='item-0'", [], |r| {
            r.get(0)
        })
        .unwrap();
    b.execute(
        "INSERT INTO highlights(article_id,quote,note) VALUES(?1,'preserved','offline note')",
        [target],
    )
    .unwrap();
    let plan = preview_feed_cleanup(&a, feed).unwrap();
    cleanup_feed_articles(&a, feed, &plan.token, true).unwrap();
    webdav::merge_remote(&mut b, &webdav::records(&a).unwrap()).unwrap();
    assert_eq!(
        b.query_row(
            "SELECT body_text FROM articles WHERE id=?1",
            [target],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "preserved body"
    );
    webdav::merge_remote(&mut a, &webdav::records(&b).unwrap()).unwrap();
    assert_eq!(
        a.query_row(
            "SELECT count(*) FROM articles WHERE guid='item-0'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        a.query_row("SELECT note FROM highlights", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "offline note"
    );
    assert_eq!(
        a.query_row(
            "SELECT count(*) FROM article_tombstones WHERE guid='item-0'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(cleanup_limited_feeds(&a).unwrap(), 0);
    webdav::merge_remote(&mut b, &webdav::records(&a).unwrap()).unwrap();
    assert_eq!(
        b.query_row("SELECT count(*) FROM highlights", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn offline_star_versus_cleanup_can_be_reviewed_without_losing_the_article() {
    let mut a = database();
    let mut b = database();
    let folder = create_folder(&a, "Star").unwrap();
    let feed = subscribe(&a, folder, "star");
    for i in 0..105 {
        a.execute("INSERT INTO articles(feed_id,guid,title,published_at) VALUES(?1,?2,'Article',datetime('2026-01-01',?3))",params![feed,format!("item-{i}"),format!("+{i} days")]).unwrap();
    }
    let original = webdav::records(&a).unwrap();
    webdav::merge_remote(&mut b, &original).unwrap();
    webdav::acknowledge(&a, &original).unwrap();
    let id: i64 = b
        .query_row("SELECT id FROM articles WHERE guid='item-0'", [], |r| {
            r.get(0)
        })
        .unwrap();
    set_starred(&b, id, true).unwrap();
    let plan = preview_feed_cleanup(&a, feed).unwrap();
    cleanup_feed_articles(&a, feed, &plan.token, false).unwrap();
    let remote = webdav::records(&a).unwrap();
    assert!(webdav::merge_remote(&mut b, &remote).is_err());
    assert!(get_article(&b, id).unwrap().is_starred);
    for conflict in webdav::conflicts(&b).unwrap() {
        webdav::resolve_conflict(&mut b, &conflict.kind, &conflict.key, "local").unwrap();
    }
    webdav::merge_remote(&mut b, &remote).unwrap();
    assert!(get_article(&b, id).unwrap().is_starred);
    webdav::merge_remote(&mut a, &webdav::records(&b).unwrap()).unwrap();
    assert!(a
        .query_row(
            "SELECT is_starred FROM articles WHERE guid='item-0'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn legacy_paths_merge_matching_descendants_without_duplicate_siblings() {
    let mut c = Connection::open_in_memory().unwrap();
    MIGRATIONS.to_version(&mut c, 27).unwrap();
    c.execute("INSERT INTO folders(id,name,parent_id) VALUES(1,'AI',NULL),(2,'公众号',1),(3,'AI/公众号',NULL),(4,'Existing',2),(5,'OldBranch',3)",[]).unwrap();
    // Before v28 the names were globally unique, so rename the matching child
    // after migration, as can happen when v2 retries partially converted data.
    let one = subscribe(&c, 4, "one");
    let two = subscribe(&c, 5, "two");
    MIGRATIONS.to_latest(&mut c).unwrap();
    webdav::upgrade_folder_scope(&mut c).unwrap();
    c.execute("UPDATE folders SET name='Existing' WHERE id=5", [])
        .unwrap();
    upgrade_legacy_folder_paths(&mut c).unwrap();
    assert_eq!(
        feed_folder_id(&c, one).unwrap(),
        feed_folder_id(&c, two).unwrap()
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM folders WHERE name='Existing'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(list_feeds(&c).unwrap().len(), 2);
}
