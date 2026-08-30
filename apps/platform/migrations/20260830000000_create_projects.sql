create table projects (
    project_id uuid primary key,
    name text not null,
    avatar text,
    constraint projects_name_valid check (
        name = btrim(name) and char_length(name) between 1 and 128
    ),
    constraint projects_avatar_valid check (
        avatar is null or (
            avatar = btrim(avatar) and char_length(avatar) between 1 and 2048
        )
    )
);

create index projects_name_idx on projects (lower(name), project_id);
