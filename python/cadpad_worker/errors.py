"""Worker errors shared by source analysis and native geometry operations."""


class DomainError(Exception):
    def __init__(self, code, message, details=None):
        self.code = code
        self.details = details or {}
        super().__init__(message)
