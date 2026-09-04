alter table projects drop constraint projects_avatar_valid;

alter table projects add constraint projects_avatar_valid check (
    avatar is null or (
        avatar = btrim(avatar) and char_length(avatar) between 1 and 800000
    )
);
