use std::borrow::Cow;

use chumsky::input::Input;
use chumsky::Parser;
use dalet::daleth::format::format;
use dalet::daleth::lexer::{full_lexer, lexer};
use dalet::daleth::parser::parser;
use dalet::daleth::types::Spanned;
use dashmap::DashMap;
use ropey::Rope;
use serde_json::Value;
use tower_lsp::jsonrpc::{Error, ErrorCode, Result};
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

struct TextDocumentItem {
    uri: Url,
    text: String,
    version: i32,
}

#[derive(Debug)]
struct Backend {
    client: Client,
    document_map: DashMap<String, Rope>,
}

impl Backend {
    async fn check_file(&self, params: TextDocumentItem) {
        self.client
            .log_message(MessageType::INFO, "run file check")
            .await;

        let rope = ropey::Rope::from_str(&params.text);

        let mut errors: Vec<Spanned<String>> = vec![];

        let (tokens, lex_errors) = lexer().parse(&params.text).into_output_errors();

        for error in lex_errors {
            errors.push((error.to_string(), error.span().to_owned()));
        }

        if let Some(tokens) = tokens {
            let parse_errors = parser()
                .parse(tokens.as_slice().spanned((0..params.text.len()).into()))
                .into_errors();

            for error in parse_errors {
                errors.push((error.to_string(), error.span().to_owned()));
            }
        }

        let diagnostics = errors
            .into_iter()
            .filter_map(|(message, span)| -> Option<Diagnostic> {
                let start_position = offset_to_position(span.start, &rope)?;
                let end_position = offset_to_position(span.end, &rope)?;
                Some(Diagnostic::new(
                    Range::new(start_position, end_position),
                    Some(DiagnosticSeverity::ERROR),
                    None,
                    None,
                    message,
                    None,
                    None,
                ))
            })
            .collect::<Vec<_>>();

        self.document_map.insert(params.uri.to_string(), rope);

        self.client
            .publish_diagnostics(params.uri.clone(), diagnostics, Some(params.version))
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "daleth-lsp".to_owned(),
                version: Some("0.1.0".to_owned()),
            }),
            offset_encoding: None,
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),

                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
                }),

                document_formatting_provider: Some(OneOf::Left(true)),

                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec!["dummy.do_something".to_string()],
                    work_done_progress_options: Default::default(),
                }),

                ..ServerCapabilities::default()
            },
        })
    }
    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "initialized!")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn execute_command(&self, _: ExecuteCommandParams) -> Result<Option<Value>> {
        self.client
            .log_message(MessageType::INFO, "command executed!")
            .await;

        match self.client.apply_edit(WorkspaceEdit::default()).await {
            Ok(res) if res.applied => self.client.log_message(MessageType::INFO, "applied").await,
            Ok(_) => self.client.log_message(MessageType::INFO, "rejected").await,
            Err(err) => self.client.log_message(MessageType::ERROR, err).await,
        }

        Ok(None)
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let rope = self.document_map.get(uri.as_str()).unwrap();

        let string = rope.to_string();
        let lexed = full_lexer().parse(&string);

        match lexed.into_result() {
            Ok(t) => Ok(Some(vec![TextEdit {
                range: Range::new(
                    offset_to_position(0, &rope).unwrap(),
                    offset_to_position(string.len(), &rope).unwrap(),
                ),
                new_text: format(&t),
            }])),
            Err(_) => Err(Error {
                code: ErrorCode::InternalError,
                message: Cow::Borrowed("Lexer error"),
                data: None,
            }),
        }
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        self.check_file(TextDocumentItem {
            uri: params.text_document.uri,
            text: std::mem::take(&mut params.content_changes[0].text),
            version: params.text_document.version,
        })
        .await;
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.client
            .log_message(MessageType::INFO, "file opened")
            .await;
        self.check_file(TextDocumentItem {
            uri: params.text_document.uri,
            text: params.text_document.text,
            version: params.text_document.version,
        })
        .await
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::build(|client| Backend {
        client,
        document_map: DashMap::new(),
    })
    .finish();

    Server::new(stdin, stdout, socket).serve(service).await;
}

fn offset_to_position(offset: usize, rope: &Rope) -> Option<Position> {
    let line = rope.try_char_to_line(offset).ok()?;
    let first_char_of_line = rope.try_line_to_char(line).ok()?;
    let column = offset - first_char_of_line;
    Some(Position::new(line as u32, column as u32))
}
