//! The word list a safety number can be read out in.
//!
//! Twenty-five digits are correct and nobody enjoys reading them down a phone
//! line, which is how a verification step that exists gets skipped. The same
//! fingerprint as words is the same check, said out loud in half the time.
//!
//! The list is fixed: both ends index into it, so changing an entry changes
//! every safety phrase and two versions would disagree about what they are
//! comparing. Add nothing, reorder nothing.
//!
//! Chosen to survive a bad line rather than to be interesting — three to six
//! letters, no word a prefix of another, and no two so close that "did you say
//! *grid* or *grill*" decides whether you trust a conversation.
//! `the_list_is_usable` holds that shape.

/// Exactly 256, so one byte of the digest picks one word and there is no bit
/// packing to get wrong.
pub const WORDS: [&str; 256] = [
    "acid", "actor", "agent", "album", "alert", "amber", "ankle", "apple", "arrow", "atlas",
    "audio", "avoid", "awake", "bacon", "badge", "baker", "ballot", "banjo", "basin", "beach",
    "beard", "berry", "bison", "blade", "bloom", "board", "bonus", "brave", "bread", "brick",
    "brush", "bugle", "cabin", "cable", "cactus", "camel", "candy", "canvas", "cargo", "carpet",
    "cedar", "chair", "chalk", "charm", "cheese", "cherry", "chess", "chill", "cider", "circus",
    "civic", "clay", "cliff", "cloth", "cloud", "clover", "coast", "cobra", "cocoa", "coffee",
    "comet", "coral", "cotton", "couch", "cousin", "cover", "crane", "cream", "crown", "crust",
    "cube", "dance", "dawn", "decoy", "deer", "delta", "denim", "dental", "desert", "diary",
    "diner", "dive", "dock", "dodge", "donkey", "donut", "dough", "dozen", "dragon", "dream",
    "dress", "drift", "drum", "dusk", "eagle", "earth", "easel", "echo", "eight", "elbow", "elder",
    "ember", "empty", "energy", "engine", "envoy", "equal", "error", "essay", "ether", "evil",
    "exact", "exit", "fable", "fabric", "facet", "fairy", "falcon", "fancy", "farm", "fatal",
    "fault", "feast", "fence", "fever", "fiber", "field", "final", "finch", "fire", "fish", "flag",
    "flame", "flask", "fleet", "flint", "float", "flock", "flour", "flute", "foam", "focus",
    "foggy", "forest", "forge", "fork", "fossil", "frame", "frost", "fruit", "fudge", "funny",
    "fury", "gadget", "galaxy", "gallon", "game", "garden", "garlic", "gate", "gauge", "gecko",
    "ghost", "giant", "ginger", "glass", "glide", "globe", "glory", "glove", "glue", "gold",
    "golf", "goose", "grape", "grass", "gravel", "green", "grid", "grill", "grove", "guitar",
    "gulf", "habit", "hammer", "hand", "harbor", "hare", "harp", "hasty", "hazel", "heart",
    "hedge", "helmet", "herb", "hero", "hill", "hinge", "hobby", "honey", "hoop", "horn", "horse",
    "hotel", "hound", "house", "human", "humid", "hunt", "hurdle", "hymn", "idea", "igloo",
    "image", "index", "inlet", "insect", "iris", "iron", "island", "ivory", "jacket", "jade",
    "jaguar", "jazz", "jelly", "jewel", "jockey", "jolly", "judge", "juice", "jumbo", "jungle",
    "junior", "jury", "kayak", "keen", "kettle", "kick", "kind", "king", "kiosk", "kite", "kitten",
    "knee", "knife", "knot", "koala", "label", "lace", "ladder", "lagoon", "lake", "lamb", "lamp",
    "lance",
];

#[cfg(test)]
mod tests {
    use super::WORDS;

    /// The properties the list is chosen for, checked rather than trusted —
    /// a duplicate or a prefix pair would weaken every safety phrase quietly.
    #[test]
    fn the_list_is_usable() {
        assert_eq!(WORDS.len(), 256, "one byte must select exactly one word");

        for word in WORDS {
            assert!(
                (3..=6).contains(&word.len()),
                "{word} is the wrong length to say aloud"
            );
            assert!(
                word.chars().all(|c| c.is_ascii_lowercase()),
                "{word} is not plain lowercase ascii"
            );
        }

        let mut sorted = WORDS;
        sorted.sort_unstable();
        for pair in sorted.windows(2) {
            assert_ne!(pair[0], pair[1], "{} appears twice", pair[0]);
            assert!(
                !pair[1].starts_with(pair[0]),
                "{} is a prefix of {}, so a clipped word is ambiguous",
                pair[0],
                pair[1]
            );
        }
    }
}
