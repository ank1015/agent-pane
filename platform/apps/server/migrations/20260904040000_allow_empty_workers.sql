-- Empty worker builds participate in lifecycle/health without claiming work.
alter table workers drop constraint workers_supported_harnesses_check;
alter table workers add constraint workers_supported_harnesses_check check (
    cardinality(supported_harnesses) = 0 or (
        array_ndims(supported_harnesses) = 1
        and array_position(supported_harnesses, null) is null
        and array_position(supported_harnesses, '') is null
    )
);
