class Error(Exception):
    prefix = "operation"
    exit_code = 1

    def __init__(self, message):
        super().__init__(message)
        self.message = message

    def __str__(self):
        return f"{self.prefix}: {self.message}"


class InputError(Error):
    prefix = "input"
    exit_code = 2


class IntegrityError(Error):
    prefix = "integrity"
    exit_code = 3


class ConflictError(Error):
    prefix = "conflict"
    exit_code = 4


class LimitError(Error):
    prefix = "limit"
    exit_code = 5


class NetworkError(Error):
    prefix = "network"
    exit_code = 6


class OperationError(Error):
    prefix = "operation"
    exit_code = 1


def http_status(error):
    if isinstance(error, InputError):
        return 400
    if isinstance(error, IntegrityError):
        return 422
    if isinstance(error, ConflictError):
        return 409
    if isinstance(error, LimitError):
        return 413
    if isinstance(error, NetworkError):
        return 502
    return 500
