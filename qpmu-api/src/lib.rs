use std::{future::Future, process};

pub use anyhow;
use anyhow::{Context, Result};
use proto::plugin_server::PluginServer;
use tokio::net::TcpListener;
use tonic::{transport::Server, Status};

pub mod rank;
pub mod sql;

mod proto {
    tonic::include_proto!("plugin");
}

pub use proto::ListItem;
impl ListItem {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            icon: None,
            description: String::new(),
            metadata: String::new(),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_metadata(mut self, meta: impl Into<String>) -> Self {
        self.metadata = meta.into();
        self
    }

    pub fn with_icon(mut self, icon: Option<impl Into<String>>) -> Self {
        self.icon = icon.map(Into::into);
        self
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Close,
    RunCommand(String, Vec<String>),
    RunShell(String),
    Copy(String),
    SetInput(Input),
}

impl Action {
    fn map_to_proto(self) -> proto::Action {
        use proto::action::Action as PrAction;

        let inner_action = match self {
            Action::Close => PrAction::Close(()),
            Action::RunCommand(cmd, args) => PrAction::RunCommand(proto::Command { cmd, args }),
            Action::RunShell(str) => PrAction::RunShell(str),
            Action::Copy(str) => PrAction::Copy(str),
            Action::SetInput(input) => PrAction::SetInput(input),
        };

        proto::Action {
            action: Some(inner_action),
        }
    }
}

pub struct SelectionRange {
    lower_bound: u16,
    upper_bound: u16,
}

impl SelectionRange {
    /// Sets both the start and end bound to the provided index.
    pub fn at(index: u16) -> Self {
        Self {
            lower_bound: index,
            upper_bound: index,
        }
    }

    /// Selects the entire query.
    pub fn all() -> Self {
        Self {
            lower_bound: 0,
            upper_bound: u16::MAX,
        }
    }

    /// Sets the start and end to `0`.
    pub fn start() -> Self {
        Self::at(0)
    }

    pub fn end() -> Self {
        Self::at(u16::MAX)
    }
}

pub use proto::Input;
impl Input {
    /// Sets the input to the provided query and with the cursor placed
    /// at the end.
    pub fn new(query: impl Into<String>) -> Self {
        let range = SelectionRange::end();
        Self {
            query: query.into(),
            range_lb: u32::from(range.lower_bound),
            range_ub: u32::from(range.upper_bound),
        }
    }

    pub fn select(mut self, sel: SelectionRange) -> Self {
        self.range_lb = u32::from(sel.lower_bound);
        self.range_ub = u32::from(sel.lower_bound);
        self
    }
}

type TonicResult<T> = Result<tonic::Response<T>, tonic::Status>;

pub trait Plugin: Sized + Send + Sync + 'static {
    fn new(toml: String) -> impl Future<Output = Result<Self>> + Send;

    fn query(&self, query: String) -> impl Future<Output = Result<Vec<ListItem>>> + Send;

    fn activate(&self, query: ListItem) -> impl Future<Output = Result<Vec<Action>>> + Send;

    fn complete(
        &self,
        _query: String,
        _selected: ListItem,
    ) -> impl Future<Output = Result<Option<Input>>> + Send {
        async { Ok(None) }
    }
}

#[tonic::async_trait]
impl<T> proto::plugin_server::Plugin for T
where
    T: Plugin,
{
    async fn query(
        &self,
        request: tonic::Request<proto::QueryRequest>,
    ) -> TonicResult<proto::QueryResponse> {
        map_result(
            T::query(self, request.into_inner().query)
                .await
                .map(|items| proto::QueryResponse { items }),
        )
    }

    async fn activate(
        &self,
        request: tonic::Request<ListItem>,
    ) -> TonicResult<proto::ActivationResponse> {
        // increment frequency table
        sqlx::query(
            "
            INSERT INTO activations (title, frequency, last_use)
            VALUES (?, 1, ?)
            ON CONFLICT (title) DO UPDATE SET
                frequency = frequency + 1,
                last_use = excluded.last_use
            ",
        )
        .bind(&request.get_ref().title)
        .bind(time::OffsetDateTime::now_utc())
        .execute(sql::pool())
        .await
        .map_err(|e| Status::unknown(e.to_string()))?;

        map_result(
            T::activate(self, request.into_inner())
                .await
                .map(|actions| proto::ActivationResponse {
                    actions: actions.into_iter().map(Action::map_to_proto).collect(),
                }),
        )
    }

    async fn complete(
        &self,
        request: tonic::Request<proto::CompletionRequest>,
    ) -> TonicResult<proto::CompletionResponse> {
        let request = request.into_inner();

        map_result(
            T::complete(
                self,
                request.query,
                request
                    .selected
                    .expect("completion request should always have an associated selected item"),
            )
            .await
            .map(|input| proto::CompletionResponse { input }),
        )
    }
}

fn map_result<T>(result: Result<T>) -> TonicResult<T> {
    match result {
        Ok(response) => Ok(tonic::Response::new(response)),
        Err(err) => Err(Status::unknown(
            err.chain()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

pub fn main<T: Plugin>() -> ! {
    let result = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow::anyhow!(e))
        .and_then(|rt| {
            rt.block_on(async {
                let mut args = std::env::args();
                let sqlite_url = args
                    .nth(1)
                    .context("missing sqlite url as first argument")?;

                let toml = args
                    .next()
                    .context("missing toml settings as second argument")?;

                sql::init(&sqlite_url).await?;

                // if port 0 is provided, asks the OS for a port
                // https://github.com/hyperium/tonic/blob/master/tests/integration_tests/tests/timeout.rs#L77-L89
                let listener = TcpListener::bind("[::1]:0").await?;
                let port = listener.local_addr()?.port();
                let plugin = T::new(toml).await?;

                // print port for qpmu to read
                println!("PORT:{port}");

                Server::builder()
                    .add_service(PluginServer::new(plugin))
                    .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                    .await?;

                Ok(())
            })
        });

    match result {
        Ok(()) => process::exit(0),
        Err(e) => {
            print_error(e);
            process::exit(1)
        }
    }
}

fn print_error(e: anyhow::Error) {
    let err_string = e
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    println!("ERROR:{err_string}");
}
