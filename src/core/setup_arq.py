from app.services import update, update_books, update_authors, update_sequences
from core.arq_pool import get_redis_settings, get_arq_pool


async def startup(ctx):
    ctx["arc_pool"] = await get_arq_pool()


class WorkerSettings:
    functions = [update, update_books, update_authors, update_sequences]
    on_startup = startup
    redis_settings = get_redis_settings()
    max_jobs = 2
