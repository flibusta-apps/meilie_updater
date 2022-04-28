from app.services import (
    update,
    update_books,
    update_authors,
    update_sequences,
    update_genres,
)
from core.arq_pool import get_redis_settings, get_arq_pool
import core.sentry  # noqa: F401


async def startup(ctx):
    ctx["arc_pool"] = await get_arq_pool()


class WorkerSettings:
    functions = [update, update_books, update_authors, update_sequences, update_genres]
    on_startup = startup
    redis_settings = get_redis_settings()
    max_jobs = 1
    job_timeout = 15 * 60
