from fastapi import FastAPI
from pydantic import BaseModel


class UserCreate(BaseModel):
    name: str


class UserResponse(BaseModel):
    id: str
    name: str


def create_app():
    """An import-safe factory also works in offline contract extraction."""
    app = FastAPI(title="Users API", version="1.0.0")

    @app.post("/users", tags=["users"], response_model=UserResponse, status_code=201)
    def create_user(user: UserCreate):
        return UserResponse(id="example", name=user.name)

    @app.get("/users/{id}", tags=["users"], response_model=UserResponse)
    def get_user(id: str):
        return UserResponse(id=id, name="Ada")

    return app


app = create_app()
