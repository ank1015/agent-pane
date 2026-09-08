pub(crate) fn generate(append: Option<&str>, web: bool, browser: bool) -> String {
    let mut prompt = include_str!("system_prompt.md").to_owned();
    prompt.push_str(if web { "\nWeb research is available in code mode: tools['web.search']({query}) finds public sources, and tools['web.scrape']({url}) reads a selected public page. Cite the returned URLs.\n" } else { "\nWeb search and scrape are not configured on this worker. They are absent from ALL_TOOLS; do not claim to have browsed sources.\n" });
    prompt.push_str(if browser { "\nThe optional browser.verify tool is configured on this worker. Inspect its returned checks and limitations.\n" } else { "\nBrowser verification is not configured on this worker. Use source review and backend invocation checks, and explicitly report that frontend browser behavior was not verified.\n" });
    prompt
        .push_str("\nPlatform SDK TypeScript reference (ctx.platform implements SitePlatform):\n");
    prompt.push_str(&platform_javascript_sdk::declarations());
    prompt.push_str(
        "\nAgent authoring SDK TypeScript reference (ctx.sites implements the sites namespace):\n",
    );
    prompt.push_str(&platform_javascript_sdk::sites_declarations());
    prompt.push_str("\nBackend handler SDK reference:\n");
    prompt.push_str(include_str!(
        "../../../../apps/sites-service/src/sdk.base.d.ts"
    ));
    if let Some(append) = append {
        prompt.push_str("\nAdditional session instructions:\n");
        prompt.push_str(append);
    }
    prompt
}
