use calcraft_core::{CollectionKind, Slug};

use super::*;
use crate::{
    Collection, DECLINE_COOLDOWN_SECS, DEFAULT_COLLECTION_SLUG, MAX_PENDING_PER_OWNER, NewItem,
    Precondition, PutOutcome, Share, ShareChanges, User,
};

struct Setup {
    store: Store,
    alice: User,
    bob: User,
    carol: User,
    dave: User,
    cal: Collection,
    group: Group,
}

async fn setup() -> Setup {
    let store = Store::in_memory().await.unwrap();
    let mut users = Vec::new();
    for name in ["alice", "bob", "carol", "dave"] {
        users.push(
            store
                .create_user(&name.parse().unwrap(), "h", None)
                .await
                .unwrap(),
        );
    }
    let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let cal = store
        .collection(users[0].id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    let group = store
        .create_group(&"team".parse().unwrap(), Some("The team"), 10, 1)
        .await
        .unwrap();
    let mut users = users.into_iter();
    Setup {
        store,
        alice: users.next().unwrap(),
        bob: users.next().unwrap(),
        carol: users.next().unwrap(),
        dave: users.next().unwrap(),
        cal,
        group,
    }
}

impl Setup {
    async fn add(&self, user: &User) -> MemberAdded {
        self.store
            .add_group_member(self.group.id, user.id, 100, 1)
            .await
            .unwrap()
    }

    async fn share_with_group(&self, access: ShareAccess, now: i64) -> GroupShareOutcome {
        self.store
            .set_group_share(self.cal.id, self.alice.id, self.group.id, access, now)
            .await
            .unwrap()
            .unwrap()
    }

    async fn incoming(&self, user: &User) -> Vec<Share> {
        self.store.incoming_shares(user.id).await.unwrap()
    }

    async fn only_share(&self, user: &User) -> Share {
        let shares = self.incoming(user).await;
        assert_eq!(shares.len(), 1, "{shares:?}");
        shares.into_iter().next().unwrap()
    }

    async fn accept(&self, user: &User) -> Share {
        let share = self.only_share(user).await;
        self.store
            .accept_share(user.id, &share.public_id, 2)
            .await
            .unwrap()
            .unwrap()
    }

    async fn visible(&self, user: &User) -> Vec<Collection> {
        self.store
            .shared_collections(user.id, CollectionKind::Calendar)
            .await
            .unwrap()
    }
}

fn item(uid: &str) -> NewItem<'_> {
    NewItem {
        uid,
        component: "VEVENT",
        range_start: None,
        range_end: None,
        recurring: false,
        index_version: 0,
        etag: "\"e\"",
        content: b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n",
        attachments: &[],
    }
}

#[tokio::test]
async fn groups_are_unique_and_limited() {
    let s = setup().await;
    assert!(matches!(
        s.store
            .create_group(&"team".parse().unwrap(), None, 10, 1)
            .await,
        Err(StoreError::GroupExists)
    ));
    s.store
        .create_group(&"other".parse().unwrap(), None, 2, 1)
        .await
        .unwrap();
    assert!(matches!(
        s.store
            .create_group(&"third".parse().unwrap(), None, 2, 1)
            .await,
        Err(StoreError::Limit(_))
    ));
    let names: Vec<String> = s
        .store
        .list_groups()
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.name.to_string())
        .collect();
    assert_eq!(names, ["other", "team"]);
    assert_eq!(
        s.store
            .group_by_name(&"team".parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .display_name
            .as_deref(),
        Some("The team")
    );
}

#[tokio::test]
async fn members_are_limited_and_listed() {
    let s = setup().await;
    assert!(s.add(&s.bob).await.added);
    assert!(!s.add(&s.bob).await.added);
    assert!(matches!(
        s.store.add_group_member(s.group.id, s.carol.id, 1, 1).await,
        Err(StoreError::Limit(_))
    ));
    assert!(matches!(
        s.store.add_group_member(9999, s.carol.id, 10, 1).await,
        Err(StoreError::NoSuchGroup)
    ));
    assert!(matches!(
        s.store.add_group_member(s.group.id, 9999, 10, 1).await,
        Err(StoreError::NoSuchSharee)
    ));
    s.add(&s.carol).await;
    let members: Vec<String> = s
        .store
        .group_members(s.group.id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.username.to_string())
        .collect();
    assert_eq!(members, ["bob", "carol"]);
    assert!(s.store.is_group_member(s.group.id, s.bob.id).await.unwrap());
    assert!(
        !s.store
            .is_group_member(s.group.id, s.dave.id)
            .await
            .unwrap()
    );
    assert_eq!(s.store.groups_of_user(s.bob.id).await.unwrap().len(), 1);
    assert!(s.store.groups_of_user(s.dave.id).await.unwrap().is_empty());
    assert!(
        s.store
            .remove_group_member(s.group.id, s.bob.id)
            .await
            .unwrap()
    );
    assert!(
        !s.store
            .remove_group_member(s.group.id, s.bob.id)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn every_member_gets_an_own_invitation() {
    let s = setup().await;
    // The owner is a member too, and gets nothing.
    for user in [&s.alice, &s.bob, &s.carol] {
        s.add(user).await;
    }
    let outcome = s.share_with_group(ShareAccess::Read, 1).await;
    assert!(outcome.created);
    assert_eq!((outcome.invited, outcome.skipped), (2, 0));
    assert_eq!(outcome.share.members, 2);
    assert!(s.incoming(&s.alice).await.is_empty());
    // Pending until each member accepts, separately.
    assert!(s.visible(&s.bob).await.is_empty());
    let bob = s.accept(&s.bob).await;
    assert_eq!(bob.groups, ["team"]);
    assert_eq!(bob.direct, None);
    assert_eq!(bob.access, ShareAccess::Read);
    assert_eq!(s.visible(&s.bob).await.len(), 1);
    assert!(s.visible(&s.carol).await.is_empty());
    assert!(!s.only_share(&s.carol).await.accepted());
    // Distinct invitations.
    assert_ne!(bob.public_id, s.only_share(&s.carol).await.public_id);
    // The owner sees the members' shares and the group share.
    assert_eq!(
        s.store.shares_of_collection(s.cal.id).await.unwrap().len(),
        2
    );
    let groups = s.store.group_shares_of_collection(s.cal.id).await.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].group.as_str(), "team");
    // Changing the access does not invite again and keeps acceptance.
    let outcome = s.share_with_group(ShareAccess::ReadWrite, 3).await;
    assert!(!outcome.created);
    assert_eq!(outcome.invited, 0);
    let bob = s.only_share(&s.bob).await;
    assert!(bob.accepted());
    assert_eq!(bob.access, ShareAccess::ReadWrite);
}

#[tokio::test]
async fn joining_invites_and_leaving_revokes_at_once() {
    let s = setup().await;
    s.share_with_group(ShareAccess::ReadWrite, 1).await;
    // Join: invitation.
    let added = s.add(&s.bob).await;
    assert_eq!(added.invited, 1);
    let share = s.accept(&s.bob).await;
    s.store
        .put_item_shared(share.id, s.cal.id, "a.ics", &item("a"), &Precondition::None)
        .await
        .unwrap();
    // Leave: gone, and a write through the old share fails.
    assert!(
        s.store
            .remove_group_member(s.group.id, s.bob.id)
            .await
            .unwrap()
    );
    assert!(s.incoming(&s.bob).await.is_empty());
    assert!(s.visible(&s.bob).await.is_empty());
    assert!(matches!(
        s.store
            .put_item_shared(share.id, s.cal.id, "b.ics", &item("b"), &Precondition::None)
            .await,
        Err(StoreError::ShareRevoked)
    ));
    // Rejoining is a new invitation (with a new id).
    assert_eq!(s.add(&s.bob).await.invited, 1);
    let again = s.only_share(&s.bob).await;
    assert!(!again.accepted());
    assert_ne!(again.public_id, share.public_id);
}

#[tokio::test]
async fn direct_and_group_shares_combine_to_the_maximum() {
    let s = setup().await;
    s.add(&s.bob).await;
    s.store
        .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::Read, 1)
        .await
        .unwrap();
    let outcome = s.share_with_group(ShareAccess::ReadWrite, 1).await;
    // The direct invitation is the one invitation.
    assert_eq!(outcome.invited, 0);
    let share = s.accept(&s.bob).await;
    assert_eq!(share.access, ShareAccess::ReadWrite);
    assert_eq!(share.direct, Some(ShareAccess::Read));
    assert_eq!(share.groups, ["team"]);
    assert!(!s.visible(&s.bob).await[0].read_only);

    // Removing the group share keeps the direct one (read only now).
    assert!(
        s.store
            .remove_group_share(s.cal.id, s.alice.id, s.group.id)
            .await
            .unwrap()
    );
    let share = s.only_share(&s.bob).await;
    assert!(share.accepted());
    assert_eq!(share.access, ShareAccess::Read);
    assert!(share.groups.is_empty());
    assert!(matches!(
        s.store
            .put_item_shared(share.id, s.cal.id, "a.ics", &item("a"), &Precondition::None)
            .await,
        Err(StoreError::ShareRevoked)
    ));

    // Group again; removing the direct share keeps the group's.
    s.share_with_group(ShareAccess::ReadWrite, 1).await;
    assert!(
        s.store
            .remove_share(s.cal.id, s.alice.id, s.bob.id)
            .await
            .unwrap()
    );
    let share = s.only_share(&s.bob).await;
    assert!(share.accepted());
    assert_eq!((share.access, share.direct), (ShareAccess::ReadWrite, None));
    // A second removal of the direct share finds nothing.
    assert!(
        !s.store
            .remove_share(s.cal.id, s.alice.id, s.bob.id)
            .await
            .unwrap()
    );
    // Leaving the group ends it.
    s.store
        .remove_group_member(s.group.id, s.bob.id)
        .await
        .unwrap();
    assert!(s.incoming(&s.bob).await.is_empty());
}

#[tokio::test]
async fn two_groups_and_one_leaves() {
    let s = setup().await;
    let other = s
        .store
        .create_group(&"other".parse().unwrap(), None, 10, 1)
        .await
        .unwrap();
    s.add(&s.bob).await;
    s.store
        .add_group_member(other.id, s.bob.id, 10, 1)
        .await
        .unwrap();
    s.share_with_group(ShareAccess::Read, 1).await;
    s.store
        .set_group_share(s.cal.id, s.alice.id, other.id, ShareAccess::ReadWrite, 1)
        .await
        .unwrap();
    let share = s.accept(&s.bob).await;
    assert_eq!(share.access, ShareAccess::ReadWrite);
    assert_eq!(share.groups, ["other", "team"]);
    // Deleting the read-write group leaves the read group's share.
    assert!(s.store.delete_group(other.id).await.unwrap());
    let share = s.only_share(&s.bob).await;
    assert!(share.accepted());
    assert_eq!(share.access, ShareAccess::Read);
    // Deleting the last group revokes.
    assert!(s.store.delete_group(s.group.id).await.unwrap());
    assert!(!s.store.delete_group(s.group.id).await.unwrap());
    assert!(s.incoming(&s.bob).await.is_empty());
    assert!(
        s.store
            .group_shares_of_collection(s.cal.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn declining_a_group_invitation_is_remembered_per_member() {
    let s = setup().await;
    s.add(&s.bob).await;
    s.add(&s.carol).await;
    s.share_with_group(ShareAccess::Read, 1).await;
    let invitation = s.only_share(&s.bob).await;
    assert!(
        s.store
            .leave_share(s.bob.id, &invitation.public_id, 10)
            .await
            .unwrap()
    );
    assert!(s.incoming(&s.bob).await.is_empty());
    // Carol still has hers.
    assert_eq!(s.incoming(&s.carol).await.len(), 1);
    // Saving the group share again, or leaving and rejoining the group,
    // does not invite bob again.
    assert_eq!(s.share_with_group(ShareAccess::Read, 20).await.invited, 0);
    s.store
        .remove_group_member(s.group.id, s.bob.id)
        .await
        .unwrap();
    assert_eq!(s.add(&s.bob).await.invited, 0);
    assert!(s.incoming(&s.bob).await.is_empty());
    // A new group share within a day: skipped (the owner's block).
    s.store
        .remove_group_share(s.cal.id, s.alice.id, s.group.id)
        .await
        .unwrap();
    let outcome = s.share_with_group(ShareAccess::Read, 20).await;
    assert_eq!((outcome.invited, outcome.skipped), (1, 1));
    assert!(s.incoming(&s.bob).await.is_empty());
    // A direct share within a day is refused as before.
    assert!(matches!(
        s.store
            .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::Read, 20)
            .await,
        Err(StoreError::ShareDeclined)
    ));
    // After the block, saving the group share invites bob.
    let later = 10 + DECLINE_COOLDOWN_SECS + 1;
    assert_eq!(
        s.share_with_group(ShareAccess::Read, later).await.invited,
        1
    );
    assert_eq!(s.incoming(&s.bob).await.len(), 1);
}

#[tokio::test]
async fn leaving_an_accepted_share_declines_the_group_share() {
    let s = setup().await;
    s.add(&s.bob).await;
    s.store
        .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::Read, 1)
        .await
        .unwrap();
    s.share_with_group(ShareAccess::Read, 1).await;
    let share = s.accept(&s.bob).await;
    // Leaving ends both grants.
    assert!(
        s.store
            .leave_share(s.bob.id, &share.public_id, 10)
            .await
            .unwrap()
    );
    assert!(s.incoming(&s.bob).await.is_empty());
    let later = 10 + DECLINE_COOLDOWN_SECS + 1;
    assert_eq!(
        s.share_with_group(ShareAccess::Read, later).await.invited,
        0
    );
    // A new direct share after the block does not bring the declined
    // group share back.
    s.store
        .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::Read, later)
        .await
        .unwrap();
    s.share_with_group(ShareAccess::ReadWrite, later).await;
    let share = s.only_share(&s.bob).await;
    assert_eq!(share.access, ShareAccess::Read);
    assert!(share.groups.is_empty());
}

#[tokio::test]
async fn group_invitations_obey_the_pending_limits() {
    let s = setup().await;
    // Alice already has the maximum of pending invitations for bob.
    for n in 0..MAX_PENDING_PER_OWNER {
        let slug: Slug = format!("c{n}").parse().unwrap();
        let cal = s
            .store
            .create_collection(
                s.alice.id,
                CollectionKind::Calendar,
                &slug,
                None,
                None,
                None,
                "VEVENT",
            )
            .await
            .unwrap();
        s.store
            .set_share(cal.id, s.alice.id, s.bob.id, ShareAccess::Read, 1)
            .await
            .unwrap();
    }
    s.add(&s.bob).await;
    s.add(&s.carol).await;
    let outcome = s.share_with_group(ShareAccess::Read, 1).await;
    assert_eq!((outcome.invited, outcome.skipped), (1, 1));
    assert_eq!(
        s.incoming(&s.bob).await.len(),
        usize::try_from(MAX_PENDING_PER_OWNER).unwrap()
    );
    // Once bob answered one, saving the group share again invites him.
    let first = s.incoming(&s.bob).await.remove(0);
    s.store
        .accept_share(s.bob.id, &first.public_id, 2)
        .await
        .unwrap();
    let outcome = s.share_with_group(ShareAccess::Read, 3).await;
    assert_eq!((outcome.invited, outcome.skipped), (1, 0));
}

#[tokio::test]
async fn group_shares_are_limited_and_owner_only() {
    let s = setup().await;
    // Not the owner's collection: nothing.
    assert!(
        s.store
            .set_group_share(s.cal.id, s.bob.id, s.group.id, ShareAccess::Read, 1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !s.store
            .remove_group_share(s.cal.id, s.bob.id, s.group.id)
            .await
            .unwrap()
    );
    assert!(matches!(
        s.store
            .set_group_share(s.cal.id, s.alice.id, 9999, ShareAccess::Read, 1)
            .await,
        Err(StoreError::NoSuchGroup)
    ));
    for n in 0..MAX_GROUP_SHARES_PER_COLLECTION - 1 {
        let group = s
            .store
            .create_group(&format!("g{n}").parse().unwrap(), None, 100, 1)
            .await
            .unwrap();
        s.store
            .set_group_share(s.cal.id, s.alice.id, group.id, ShareAccess::Read, 1)
            .await
            .unwrap()
            .unwrap();
    }
    s.share_with_group(ShareAccess::Read, 1).await;
    let last = s
        .store
        .create_group(&"last".parse().unwrap(), None, 100, 1)
        .await
        .unwrap();
    assert!(matches!(
        s.store
            .set_group_share(s.cal.id, s.alice.id, last.id, ShareAccess::Read, 1)
            .await,
        Err(StoreError::Limit(_))
    ));
    // A subscription is shared read-only only.
    s.store.mark_read_only_for_tests(s.cal.id).await.unwrap();
    assert!(matches!(
        s.store
            .set_group_share(s.cal.id, s.alice.id, s.group.id, ShareAccess::ReadWrite, 1)
            .await,
        Err(StoreError::ReadOnly)
    ));
}

#[tokio::test]
async fn disabled_members_see_nothing_until_enabled() {
    let s = setup().await;
    s.add(&s.bob).await;
    s.share_with_group(ShareAccess::Read, 1).await;
    s.accept(&s.bob).await;
    s.store.set_disabled(&s.bob.username, true).await.unwrap();
    assert!(s.visible(&s.bob).await.is_empty());
    assert!(
        s.store
            .shares_of_collection(s.cal.id)
            .await
            .unwrap()
            .is_empty()
    );
    // A disabled user who joins is invited, hidden while disabled.
    s.store.set_disabled(&s.carol.username, true).await.unwrap();
    assert_eq!(s.add(&s.carol).await.invited, 1);
    assert!(s.incoming(&s.carol).await.is_empty());
    s.store.set_disabled(&s.bob.username, false).await.unwrap();
    s.store
        .set_disabled(&s.carol.username, false)
        .await
        .unwrap();
    assert_eq!(s.visible(&s.bob).await.len(), 1);
    assert_eq!(s.incoming(&s.carol).await.len(), 1);
}

#[tokio::test]
async fn several_changes_with_groups_are_all_or_nothing() {
    let s = setup().await;
    s.add(&s.bob).await;
    // The second group does not exist: nothing changes.
    assert!(matches!(
        s.store
            .apply_share_changes(
                s.cal.id,
                s.alice.id,
                &ShareChanges {
                    set: &[(s.dave.id, ShareAccess::Read)],
                    set_groups: &[(s.group.id, ShareAccess::Read), (9999, ShareAccess::Read)],
                    ..ShareChanges::default()
                },
                1,
            )
            .await,
        Err(StoreError::NoSuchGroup)
    ));
    assert!(
        s.store
            .shares_of_collection(s.cal.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.store
            .group_shares_of_collection(s.cal.id)
            .await
            .unwrap()
            .is_empty()
    );
    s.store
        .apply_share_changes(
            s.cal.id,
            s.alice.id,
            &ShareChanges {
                set_groups: &[(s.group.id, ShareAccess::Read)],
                ..ShareChanges::default()
            },
            1,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.incoming(&s.bob).await.len(), 1);
    s.store
        .apply_share_changes(
            s.cal.id,
            s.alice.id,
            &ShareChanges {
                remove_groups: &[s.group.id],
                ..ShareChanges::default()
            },
            1,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(s.incoming(&s.bob).await.is_empty());
}

#[tokio::test]
async fn removing_a_member_never_touches_the_owners_data() {
    let s = setup().await;
    s.add(&s.bob).await;
    s.share_with_group(ShareAccess::Read, 1).await;
    s.accept(&s.bob).await;
    // The owner's object is not touched by a member's removal.
    let outcome = s
        .store
        .put_item(s.cal.id, "a.ics", &item("a"), &Precondition::None)
        .await
        .unwrap();
    assert_eq!(outcome, PutOutcome::Created);
    s.store
        .remove_group_member(s.group.id, s.bob.id)
        .await
        .unwrap();
    assert_eq!(s.store.items(s.cal.id).await.unwrap().len(), 1);
}
