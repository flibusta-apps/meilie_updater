use serde::{Deserialize, Serialize};
use tokio_postgres::Row;

pub trait UpdateModel {
    fn get_index() -> String;
    fn get_query() -> String;
    fn from_row(row: Row) -> Result<Self, tokio_postgres::Error>
    where
        Self: Sized;
    fn get_searchable_attributes() -> &'static [&'static str];
    fn get_filterable_attributes() -> &'static [&'static str];
    fn get_ranking_rules() -> &'static [&'static str];
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Book {
    pub id: i32,
    pub title: String,
    pub lang: String,
    pub genres: Vec<i32>,
}

impl UpdateModel for Book {
    fn get_index() -> String {
        "books".to_string()
    }

    fn get_query() -> String {
        "SELECT id, title, lang, array(SELECT genre FROM book_genres WHERE book = books.id) FROM books WHERE is_deleted = 'f';".to_string()
    }

    fn from_row(row: Row) -> Result<Self, tokio_postgres::Error> {
        Ok(Self {
            id: row.try_get(0)?,
            title: row.try_get(1)?,
            lang: row.try_get(2)?,
            genres: row.try_get(3)?,
        })
    }

    fn get_searchable_attributes() -> &'static [&'static str] {
        &["title"]
    }

    fn get_filterable_attributes() -> &'static [&'static str] {
        &["lang", "genres"]
    }

    fn get_ranking_rules() -> &'static [&'static str] {
        &[
            "words",
            "typo",
            "proximity",
            "attribute",
            "sort",
            "exactness",
        ]
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Author {
    pub id: i32,
    pub first_name: String,
    pub last_name: String,
    pub middle_name: String,
    pub author_langs: Vec<String>,
    pub translator_langs: Vec<String>,
    pub books_count: i64,
}

impl UpdateModel for Author {
    fn get_index() -> String {
        "authors".to_string()
    }

    fn get_query() -> String {
        "
        SELECT id, first_name, last_name, middle_name,
        array(
          SELECT DISTINCT lang FROM book_authors
          LEFT JOIN books ON book = books.id
          WHERE authors.id = book_authors.author
          AND books.is_deleted = 'f'
        ) AS author_langs,
        array(
          SELECT DISTINCT lang FROM translations
          LEFT JOIN books ON book = books.id
          WHERE authors.id = translations.author
          AND books.is_deleted = 'f'
        ) AS translator_langs,
        (
          SELECT count(books.id) FROM book_authors
          LEFT JOIN books ON book = books.id
          WHERE authors.id = book_authors.author
          AND books.is_deleted = 'f'
        ) AS books_count
        FROM authors;
        "
        .to_string()
    }

    fn from_row(row: Row) -> Result<Self, tokio_postgres::Error> {
        Ok(Self {
            id: row.try_get(0)?,
            first_name: row.try_get(1)?,
            last_name: row.try_get(2)?,
            middle_name: row.try_get(3)?,
            author_langs: row.try_get(4)?,
            translator_langs: row.try_get(5)?,
            books_count: row.try_get(6)?,
        })
    }

    fn get_searchable_attributes() -> &'static [&'static str] {
        &["first_name", "last_name", "middle_name"]
    }

    fn get_filterable_attributes() -> &'static [&'static str] {
        &["author_langs", "translator_langs"]
    }

    fn get_ranking_rules() -> &'static [&'static str] {
        &[
            "words",
            "typo",
            "proximity",
            "attribute",
            "sort",
            "exactness",
            "books_count:desc",
        ]
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Sequence {
    pub id: i32,
    pub name: String,
    pub langs: Vec<String>,
    pub books_count: i64,
}

impl UpdateModel for Sequence {
    fn get_index() -> String {
        "sequences".to_string()
    }

    fn get_query() -> String {
        "
        SELECT id, name,
          array(
            SELECT DISTINCT lang FROM book_sequences
            LEFT JOIN books ON book = books.id
            WHERE sequences.id = book_sequences.sequence
              AND books.is_deleted = 'f'
          ) as langs,
          (SELECT count(books.id) FROM book_sequences
           LEFT JOIN books ON book = books.id
           WHERE sequences.id = book_sequences.sequence
             AND books.is_deleted = 'f') as books_count
        FROM sequences;
        "
        .to_string()
    }

    fn from_row(row: Row) -> Result<Self, tokio_postgres::Error> {
        Ok(Self {
            id: row.try_get(0)?,
            name: row.try_get(1)?,
            langs: row.try_get(2)?,
            books_count: row.try_get(3)?,
        })
    }

    fn get_searchable_attributes() -> &'static [&'static str] {
        &["name"]
    }

    fn get_filterable_attributes() -> &'static [&'static str] {
        &["langs"]
    }

    fn get_ranking_rules() -> &'static [&'static str] {
        &[
            "words",
            "typo",
            "proximity",
            "attribute",
            "sort",
            "exactness",
            "books_count:desc",
        ]
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Genre {
    pub id: i32,
    pub description: String,
    pub meta: String,
    pub langs: Vec<String>,
    pub books_count: i64,
}

impl UpdateModel for Genre {
    fn get_index() -> String {
        "genres".to_string()
    }

    fn get_query() -> String {
        "
        SELECT id, description, meta,
        array(
            SELECT DISTINCT lang FROM book_genres
            LEFT JOIN books ON book = books.id
            WHERE genres.id = book_genres.genre
            AND books.is_deleted = 'f'
        ) as langs,
        (
            SELECT count(*) FROM book_genres
            LEFT JOIN books ON book = books.id
            WHERE genres.id = book_genres.genre
            AND books.is_deleted = 'f'
        ) as books_count
        FROM genres;
        "
        .to_string()
    }

    fn from_row(row: Row) -> Result<Self, tokio_postgres::Error> {
        Ok(Self {
            id: row.try_get(0)?,
            description: row.try_get(1)?,
            meta: row.try_get(2)?,
            langs: row.try_get(3)?,
            books_count: row.try_get(4)?,
        })
    }

    fn get_searchable_attributes() -> &'static [&'static str] {
        &["description"]
    }

    fn get_filterable_attributes() -> &'static [&'static str] {
        &["langs"]
    }

    fn get_ranking_rules() -> &'static [&'static str] {
        &[
            "words",
            "typo",
            "proximity",
            "attribute",
            "sort",
            "exactness",
            "books_count:desc",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_contract() {
        assert_eq!(Book::get_index(), "books");
        assert_eq!(Book::get_searchable_attributes(), &["title"]);
        assert_eq!(Book::get_filterable_attributes(), &["lang", "genres"]);
        assert_eq!(
            Book::get_ranking_rules(),
            &[
                "words",
                "typo",
                "proximity",
                "attribute",
                "sort",
                "exactness",
            ]
        );
    }

    #[test]
    fn book_serde_round_trip() {
        let book = Book {
            id: 1,
            title: "Some Title".to_string(),
            lang: "en".to_string(),
            genres: vec![1, 2, 3],
        };

        let json = serde_json::to_string(&book).unwrap();
        let round_tripped: Book = serde_json::from_str(&json).unwrap();

        assert_eq!(book, round_tripped);
    }

    #[test]
    fn author_contract() {
        assert_eq!(Author::get_index(), "authors");
        assert_eq!(
            Author::get_searchable_attributes(),
            &["first_name", "last_name", "middle_name"]
        );
        assert_eq!(
            Author::get_filterable_attributes(),
            &["author_langs", "translator_langs"]
        );
        assert_eq!(
            Author::get_ranking_rules(),
            &[
                "words",
                "typo",
                "proximity",
                "attribute",
                "sort",
                "exactness",
                "books_count:desc",
            ]
        );
    }

    #[test]
    fn author_serde_round_trip() {
        let author = Author {
            id: 1,
            first_name: "First".to_string(),
            last_name: "Last".to_string(),
            middle_name: "Middle".to_string(),
            author_langs: vec!["en".to_string()],
            translator_langs: vec!["ru".to_string()],
            books_count: 5,
        };

        let json = serde_json::to_string(&author).unwrap();
        let round_tripped: Author = serde_json::from_str(&json).unwrap();

        assert_eq!(author, round_tripped);
    }

    #[test]
    fn sequence_contract() {
        assert_eq!(Sequence::get_index(), "sequences");
        assert_eq!(Sequence::get_searchable_attributes(), &["name"]);
        assert_eq!(Sequence::get_filterable_attributes(), &["langs"]);
        assert_eq!(
            Sequence::get_ranking_rules(),
            &[
                "words",
                "typo",
                "proximity",
                "attribute",
                "sort",
                "exactness",
                "books_count:desc",
            ]
        );
    }

    #[test]
    fn sequence_serde_round_trip() {
        let sequence = Sequence {
            id: 1,
            name: "Some Sequence".to_string(),
            langs: vec!["en".to_string()],
            books_count: 3,
        };

        let json = serde_json::to_string(&sequence).unwrap();
        let round_tripped: Sequence = serde_json::from_str(&json).unwrap();

        assert_eq!(sequence, round_tripped);
    }

    #[test]
    fn genre_contract() {
        assert_eq!(Genre::get_index(), "genres");
        assert_eq!(Genre::get_searchable_attributes(), &["description"]);
        assert_eq!(Genre::get_filterable_attributes(), &["langs"]);
        assert_eq!(
            Genre::get_ranking_rules(),
            &[
                "words",
                "typo",
                "proximity",
                "attribute",
                "sort",
                "exactness",
                "books_count:desc",
            ]
        );
    }

    #[test]
    fn genre_serde_round_trip() {
        let genre = Genre {
            id: 1,
            description: "Some Genre".to_string(),
            meta: "meta".to_string(),
            langs: vec!["en".to_string()],
            books_count: 7,
        };

        let json = serde_json::to_string(&genre).unwrap();
        let round_tripped: Genre = serde_json::from_str(&json).unwrap();

        assert_eq!(genre, round_tripped);
    }
}
