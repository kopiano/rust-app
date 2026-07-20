use chrono::Utc;
use uuid::Uuid;

pub fn asset_directory_name(username: &str, id: Uuid) -> String {
    let username = username
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                Some(character.to_ascii_lowercase())
            } else {
                Some('-')
            }
        })
        .collect::<String>();
    let username = username.trim_matches('-').trim_matches('_');
    let username = if username.is_empty() {
        "user"
    } else {
        username
    };
    format!("{}-{}-{}", Utc::now().format("%Y-%m-%d"), username, id)
}

#[cfg(test)]
mod tests {
    use super::asset_directory_name;
    use uuid::Uuid;

    #[test]
    fn creates_date_username_uuid_directory_name() {
        let id = Uuid::nil();
        let directory = asset_directory_name("Shville", id);

        assert!(directory.ends_with("-shville-00000000-0000-0000-0000-000000000000"));
        assert_eq!(directory.len(), 55);
    }

    #[test]
    fn sanitizes_username_for_a_single_path_component() {
        let directory = asset_directory_name("../Shville User", Uuid::nil());

        assert!(directory.contains("-shville-user-"));
        assert!(!directory.contains('/'));
        assert!(!directory.contains(".."));
    }
}
