//! 256 short, distinct, easy-to-type English words: one word = one byte of a pairing secret.

pub const WORDS: [&str; 256] = [
    "acid", "acorn", "actor", "adobe", "agent", "alarm", "album", "alley", "amber", "angle",
    "apple", "apron", "arena", "arrow", "aspen", "atlas", "attic", "audio", "badge", "bagel",
    "baker", "bamboo", "banjo", "barn", "basil", "beach", "beard", "bell", "bench", "berry",
    "bike", "bird", "blade", "blank", "blaze", "bloom", "board", "boat", "bolt", "bone", "book",
    "boot", "brave", "bread", "brick", "brook", "brush", "bucket", "cabin", "cable", "cactus",
    "cake", "camel", "camp", "candy", "canoe", "cape", "card", "cargo", "carrot", "castle",
    "cedar", "chair", "chalk", "charm", "cheese", "cherry", "chess", "chief", "cider", "cliff",
    "clock", "cloud", "clover", "coast", "cobra", "cocoa", "comet", "coral", "corn", "cotton",
    "crane", "crate", "creek", "crown", "cube", "curry", "daisy", "dance", "delta", "denim",
    "desk", "dial", "diary", "dice", "dingo", "disk", "dock", "dolphin", "donut", "dove", "dragon",
    "drum", "duck", "dune", "eagle", "earth", "easel", "echo", "elbow", "elder", "elm", "ember",
    "engine", "fable", "falcon", "farm", "feast", "fern", "ferry", "fiber", "fig", "film", "finch",
    "flag", "flame", "flute", "foam", "forest", "fork", "fox", "frog", "frost", "fudge", "galaxy",
    "garden", "garlic", "gate", "gecko", "gem", "ghost", "ginger", "glacier", "globe", "glove",
    "goat", "gold", "grape", "gravel", "guitar", "gull", "hammer", "harbor", "harp", "hawk",
    "hazel", "heart", "hedge", "helmet", "hill", "honey", "hood", "horse", "hotel", "husky",
    "igloo", "inch", "ink", "iris", "iron", "island", "ivory", "jacket", "jade", "jam", "jazz",
    "jelly", "jewel", "judge", "juice", "jungle", "kayak", "kettle", "key", "kite", "kiwi", "knot",
    "koala", "ladder", "lake", "lamp", "lemon", "lens", "lilac", "lime", "linen", "lion", "llama",
    "lobster", "lotus", "lunar", "mango", "maple", "marble", "mask", "meadow", "melon", "metal",
    "mint", "mirror", "moon", "moss", "motor", "mouse", "mug", "music", "nest", "noble", "noodle",
    "north", "oak", "oasis", "ocean", "olive", "onion", "opal", "orbit", "otter", "oven", "owl",
    "paddle", "palm", "panda", "paper", "parrot", "peach", "pearl", "pepper", "piano", "pilot",
    "pine", "pixel", "plum", "polar", "pony", "puzzle", "quartz", "quilt", "rabbit", "radio",
    "rain", "raven", "reef", "rice", "river", "robin",
];

pub fn index_of(word: &str) -> Option<u8> {
    let w = word.trim().to_ascii_lowercase();
    WORDS.iter().position(|x| *x == w).map(|i| i as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_unique_and_lowercase() {
        let mut v: Vec<&str> = WORDS.to_vec();
        v.sort();
        v.dedup();
        assert_eq!(v.len(), 256);
        assert!(
            WORDS
                .iter()
                .all(|w| w.chars().all(|c| c.is_ascii_lowercase()))
        );
        assert_eq!(index_of(" Apple "), Some(10));
    }
}
