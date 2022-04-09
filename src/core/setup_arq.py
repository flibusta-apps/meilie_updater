from arq.cron import cron

from app.services import (
    update,
    update_books,
    update_authors,
    update_sequences,
    update_genres,
)
from core.arq_pool import get_redis_settings, get_arq_pool


async def startup(ctx):
    ctx["arc_pool"] = await get_arq_pool()


class WorkerSettings:
    functions = [update, update_books, update_authors, update_sequences, update_genres]
    on_startup = startup
    redis_settings = get_redis_settings()
    max_jobs = 2
    job_timeout = 15 * 60
    cron_jobs = [cron(update, hour={4}, minute=0)]
