import asyncio
import concurrent.futures

from arq.connections import ArqRedis
import asyncpg
from meilisearch import Client

from core.config import env_config


def get_meilisearch_client() -> Client:
    return Client(url=env_config.MEILI_HOST, api_key=env_config.MEILI_MASTER_KEY)


async def get_postgres_pool() -> asyncpg.Pool:
    return await asyncpg.create_pool(  # type: ignore
        database=env_config.POSTGRES_DB_NAME,
        host=env_config.POSTGRES_HOST,
        port=env_config.POSTGRES_PORT,
        user=env_config.POSTGRES_USER,
        password=env_config.POSTGRES_PASSWORD,
    )


DEFAULT_RANKING_RULES = [
    "words",
    "typo",
    "proximity",
    "attribute",
    "sort",
    "exactness",
]


async def update_books(ctx) -> bool:
    loop = asyncio.get_event_loop()

    meili = get_meilisearch_client()
    index = meili.index("books")

    postgres_pool = await get_postgres_pool()

    with concurrent.futures.ThreadPoolExecutor() as pool:
        count = await postgres_pool.fetchval(
            "SELECT count(*) FROM books WHERE is_deleted = 'f';"
        )

        for offset in range(0, count, 4096):
            rows = await postgres_pool.fetch(
                "SELECT id, title, lang FROM books "
                f"ORDER BY id LIMIT 4096 OFFSET {offset}"
            )

            documents = [dict(row) for row in rows]

            await loop.run_in_executor(pool, index.add_documents, documents)

    index.update_searchable_attributes(["title"])
    index.update_filterable_attributes(["lang"])

    return True


async def update_authors(ctx) -> bool:
    loop = asyncio.get_event_loop()

    meili = get_meilisearch_client()
    index = meili.index("authors")

    postgres_pool = await get_postgres_pool()

    with concurrent.futures.ThreadPoolExecutor() as pool:
        count = await postgres_pool.fetchval("SELECT count(*) FROM authors;")

        for offset in range(0, count, 4096):
            rows = await postgres_pool.fetch(
                "SELECT id, first_name, last_name, middle_name, "
                "  array("
                "      SELECT DISTINCT lang FROM book_authors "
                "      LEFT JOIN books ON book = books.id "
                "      WHERE authors.id = book_authors.author "
                "        AND books.is_deleted = 'f' "
                "    ) as author_langs, "
                "  array("
                "      SELECT DISTINCT lang FROM translations "
                "      LEFT JOIN books ON book = books.id "
                "      WHERE authors.id = translations.author "
                "        AND books.is_deleted = 'f' "
                "    ) as translator_langs, "
                "  (SELECT count(books.id) FROM book_authors "
                "    LEFT JOIN books ON book = books.id "
                "    WHERE authors.id = book_authors.author "
                "      AND books.is_deleted = 'f') as books_count "
                "FROM authors "
                f"ORDER BY id LIMIT 4096 OFFSET {offset}"
            )

            documents = [dict(row) for row in rows]

            await loop.run_in_executor(pool, index.add_documents, documents)

    index.update_searchable_attributes(["first_name", "last_name", "middle_name"])
    index.update_filterable_attributes(["author_langs", "translator_langs"])
    index.update_ranking_rules([*DEFAULT_RANKING_RULES, "books_count:desc"])

    return True


async def update_sequences(ctx) -> bool:
    loop = asyncio.get_event_loop()

    meili = get_meilisearch_client()
    index = meili.index("sequences")

    postgres_pool = await get_postgres_pool()

    with concurrent.futures.ThreadPoolExecutor() as pool:
        count = await postgres_pool.fetchval("SELECT count(*) FROM sequences;")

        for offset in range(0, count, 4096):
            rows = await postgres_pool.fetch(
                "SELECT id, name, "
                "  array("
                "    SELECT DISTINCT lang FROM book_sequences "
                "    LEFT JOIN books ON book = books.id "
                "    WHERE sequences.id = book_sequences.sequence "
                "      AND books.is_deleted = 'f' "
                "  ) as langs, "
                "  (SELECT count(books.id) FROM book_sequences "
                "   LEFT JOIN books ON book = books.id "
                "   WHERE sequences.id = book_sequences.sequence "
                "     AND books.is_deleted = 'f') as books_count "
                "FROM sequences "
                f"ORDER BY id LIMIT 4096 OFFSET {offset}"
            )

            documents = [dict(row) for row in rows]

            await loop.run_in_executor(pool, index.add_documents, documents)

    index.update_searchable_attributes(["name"])
    index.update_filterable_attributes(["langs"])
    index.update_ranking_rules([*DEFAULT_RANKING_RULES, "books_count:desc"])

    return True


async def update(ctx) -> bool:
    arq_pool: ArqRedis = ctx["arc_pool"]

    await arq_pool.enqueue_job("update_books")
    await arq_pool.enqueue_job("update_authors")
    await arq_pool.enqueue_job("update_sequences")

    return True
