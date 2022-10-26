FROM ghcr.io/flibusta-apps/base_docker_images:3.11-postgres-asyncpg-poetry-buildtime as build-image

WORKDIR /root/poetry
COPY pyproject.toml poetry.lock /root/poetry/

ENV VENV_PATH=/opt/venv
RUN poetry export --without-hashes > requirements.txt \
    && . "${VENV_PATH}/bin/activate" \
    && pip install -r requirements.txt --no-cache-dir


FROM ghcr.io/flibusta-apps/base_docker_images:3.11-postgres-runtime as runtime-image

ENV VENV_PATH=/opt/venv
ENV PATH="$VENV_PATH/bin:$PATH"

WORKDIR /app/

COPY --from=build-image $VENV_PATH $VENV_PATH
COPY ./src/ /app/
COPY ./scripts/healthcheck.py /root/healthcheck.py

EXPOSE 8080

CMD gunicorn -k uvicorn.workers.UvicornWorker main:app --bind 0.0.0.0:8080
