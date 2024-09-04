from logging import info

from fixtures.neon_fixtures import NeonEnv


# basic test for the endpoint that returns the list of installed extensions
def test_installed_extensions(neon_simple_env: NeonEnv):
    env = neon_simple_env

    env.neon_cli.create_branch("test_installed_extensions")

    endpoint = env.endpoints.create_start("test_installed_extensions")

    endpoint.safe_psql("CREATE DATABASE test_installed_extensions")
    endpoint.safe_psql("CREATE DATABASE test_installed_extensions_2")

    client = endpoint.http_client()
    res = client.extensions()

    info("Extensions list: %s", res)
    info("Extensions: %s", res["extensions"])
    # 'plpgsql' is a default extension that is always installed.
    assert any(
        ext["extname"] == "plpgsql" and ext["highest_version"] == "1.0" for ext in res["extensions"]
    ), "The 'plpgsql' extension is missing"

    # check that the neon_test_utils extension is not installed
    assert not any(
        ext["extname"] == "neon_test_utils" for ext in res["extensions"]
    ), "The 'neon_test_utils' extension is installed"

    pg_conn = endpoint.connect(dbname="test_installed_extensions")
    with pg_conn.cursor() as cur:
        cur.execute("CREATE EXTENSION neon_test_utils")

    with pg_conn.cursor() as cur:
        cur.execute("CREATE EXTENSION neon version '1.1'")

    pg_conn_2 = endpoint.connect(dbname="test_installed_extensions_2")
    with pg_conn_2.cursor() as cur:
        cur.execute("CREATE EXTENSION neon version '1.2'")

    res = client.extensions()

    info("Extensions list: %s", res)
    info("Extensions: %s", res["extensions"])

    # check that the neon_test_utils extension is installed only in 1 database
    assert any(
        ext["extname"] == "neon_test_utils"
        and ext["lowest_version"] == "1.3"
        and ext["highest_version"] == "1.3"
        and ext["n_databases"] == 1
        for ext in res["extensions"]
    ), "The 'neon_test_utils' extension is missing"

    # check that the plpgsql extension is installed in all databases
    # this is a default extension that is always installed
    assert any(
        ext["extname"] == "plpgsql" and ext["n_databases"] == 4 for ext in res["extensions"]
    ), "The 'plpgsql' extension is missing"

    # check that the neon extension is installed and has expected versions
    assert any(
        ext["extname"] == "neon"
        and ext["lowest_version"] == "1.1"
        and ext["highest_version"] == "1.2"
        and ext["n_databases"] == 2
        for ext in res["extensions"]
    ), "The 'neon' extension is missing"

    with pg_conn.cursor() as cur:
        cur.execute("ALTER EXTENSION neon UPDATE TO '1.3'")

    res = client.extensions()

    info("Extensions list: %s", res)
    info("Extensions: %s", res["extensions"])

    # check that the neon_test_utils extension is installed
    assert any(
        ext["extname"] == "neon" and ext["highest_version"] == "1.3" for ext in res["extensions"]
    ), "The 'neon' extension is missing"
