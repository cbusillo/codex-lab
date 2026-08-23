import pytest
from app_server_harness import MockResponsesServer


@pytest.mark.parametrize("error_type", [BrokenPipeError, ConnectionResetError])
def test_expected_client_disconnect_errors_are_silent(
    error_type: type[ConnectionError],
    capsys: pytest.CaptureFixture[str],
) -> None:
    with MockResponsesServer() as responses:
        try:
            raise error_type("expected client disconnect")
        except error_type:
            responses._server.handle_error(responses._server.socket, ("127.0.0.1", 0))

    captured = capsys.readouterr()
    assert (captured.out, captured.err) == ("", "")


def test_unexpected_server_errors_use_default_error_path(
    capsys: pytest.CaptureFixture[str],
) -> None:
    with MockResponsesServer() as responses:
        try:
            raise RuntimeError("unexpected harness failure")
        except RuntimeError:
            responses._server.handle_error(responses._server.socket, ("127.0.0.1", 0))

    captured = capsys.readouterr()
    assert captured.out == ""
    assert "RuntimeError: unexpected harness failure" in captured.err
