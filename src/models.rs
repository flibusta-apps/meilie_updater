use serde::{Deserialize, Serialize};
use tokio_postgres::Row;

pub trait UpdateModel {
    fn get_index() -> String;
    fn get_query() -> String;
    fn from_row(row: Row) -> Self;
    fn get_searchanble_attributes() -> Vec<String>;
    fn get_filterable_attributes() -> Vec<String>;
    fn get_ranking_rules() -> Vec<String>;
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Book {
    pub id: i32,
    pub title: String,
    pub lang: String,
    pub genres: Vec<i32>
}

impl UpdateModel for Book {
    fn get_index() -> String {
        "books".to_string()
    }

    fn get_query() -> String {
        "SELECT id, title, lang, array(SELECT id FROM book_genres WHERE book = books.id) FROM books WHERE is_deleted = 'f';".to_string()
    }

    fn from_row(row: Row) -> Self {
        Self {
            id: row.get(0),
            title: row.get(1),
            lang: row.get(2),
            genres: row.get(3)
        }
    }

    fn get_searchanble_attributes() -> Vec<String> {
        vec!["title".to_string()]
    }

    fn get_filterable_attributes() -> Vec<String> {
        vec![
            "lang".to_string(),
            "genres".to_string()
        ]
    }

    fn get_ranking_rules() -> Vec<String> {
        vec![
            "words".to_string(),
            "typo".to_string(),
            "proximity".to_string(),
            "attribute".to_string(),
            "sort".to_string(),
            "exactness".to_string(),
        ]
    }
}


#[derive(Serialize, Deserialize, Debug)]
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

    fn from_row(row: Row) -> Self {
        Self {
            id: row.get(0),
            first_name: row.get(1),
            last_name: row.get(2),
            middle_name: row.get(3),
            author_langs: row.get(4),
            translator_langs: row.get(5),
            books_count: row.get(6),
        }
    }

    fn get_searchanble_attributes() -> Vec<String> {
        vec![
            "first_name".to_string(),
            "last_name".to_string(),
            "middle_name".to_string(),
        ]
    }

    fn get_filterable_attributes() -> Vec<String> {
        vec!["author_langs".to_string(), "translator_langs".to_string()]
    }

    fn get_ranking_rules() -> Vec<String> {
        vec![
            "words".to_string(),
            "typo".to_string(),
            "proximity".to_string(),
            "attribute".to_string(),
            "sort".to_string(),
            "exactness".to_string(),
            "books_count:desc".to_string()
        ]
    }
}


#[derive(Serialize, Deserialize, Debug)]
pub struct Sequence {
    pub id: i32,
    pub name: String,
    pub langs: Vec<String>,
    pub books_count: i64
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
        ".to_string()
    }

    fn from_row(row: Row) -> Self {
        Self {
            id: row.get(0),
            name: row.get(1),
            langs: row.get(2),
            books_count: row.get(3)
        }
    }

    fn get_searchanble_attributes() -> Vec<String> {
        vec!["name".to_string()]
    }

    fn get_filterable_attributes() -> Vec<String> {
        vec!["langs".to_string()]
    }

    fn get_ranking_rules() -> Vec<String> {
        vec![
            "words".to_string(),
            "typo".to_string(),
            "proximity".to_string(),
            "attribute".to_string(),
            "sort".to_string(),
            "exactness".to_string(),
            "books_count:desc".to_string()
        ]
    }
}


#[derive(Serialize, Deserialize, Debug)]
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
        ".to_string()
    }

    fn from_row(row: Row) -> Self {
        Self {
            id: row.get(0),
            description: row.get(1),
            meta: row.get(2),
            langs: row.get(3),
            books_count: row.get(4)
        }
    }

    fn get_searchanble_attributes() -> Vec<String> {
        vec!["description".to_string()]
    }

    fn get_filterable_attributes() -> Vec<String> {
        vec!["langs".to_string()]
    }

    fn get_ranking_rules() -> Vec<String> {
        vec![
            "words".to_string(),
            "typo".to_string(),
            "proximity".to_string(),
            "attribute".to_string(),
            "sort".to_string(),
            "exactness".to_string(),
            "books_count:desc".to_string()
        ]
    }
}
