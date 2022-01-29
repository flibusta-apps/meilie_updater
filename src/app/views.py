from fastapi import APIRouter, Depends, Request

from arq.connections import ArqRedis

from app.depends import check_token


router = APIRouter(prefix="/api/v1", dependencies=[Depends(check_token)])


@router.post("/update")
async def update(request: Request):
    arq_pool: ArqRedis = request.app.state.arq_pool
    await arq_pool.enqueue_job("update")

    return "Ok!"
