use crate::context::{Data, DeferFutureVec, EnvironmentInner};
use crate::error::ParseRequestError;
use crate::mutation_resolver::do_mutation_resolve;
use crate::registry::CacheControl;
use crate::validation::{check_rules, CheckResult};
use crate::{do_resolve, ContextBase, Environment, Error, Result, Schema, SubscriptionType};
use crate::{ObjectType, QueryError, Variables};
use futures::lock::Mutex;
use futures::{Stream, StreamExt};
use graphql_parser::query::{
    Definition, Document, OperationDefinition, SelectionSet, VariableDefinition,
};
use graphql_parser::{parse_query, Pos};
use itertools::Itertools;
use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tempdir::TempDir;

/// IntoQueryBuilder options
#[derive(Default, Clone)]
pub struct IntoQueryBuilderOpts {
    /// A temporary path to store the contents of all files.
    ///
    /// If None, the system temporary path is used.
    pub temp_dir: Option<PathBuf>,

    /// Maximum file size.
    pub max_file_size: Option<usize>,

    /// Maximum number of files.
    pub max_num_files: Option<usize>,
}

#[allow(missing_docs)]
#[async_trait::async_trait]
pub trait IntoQueryBuilder: Sized {
    async fn into_query_builder(self) -> std::result::Result<QueryBuilder, ParseRequestError> {
        self.into_query_builder_opts(&Default::default()).await
    }

    async fn into_query_builder_opts(
        self,
        opts: &IntoQueryBuilderOpts,
    ) -> std::result::Result<QueryBuilder, ParseRequestError>;
}

/// Query response
pub struct QueryResponse {
    /// Path for subsequent response
    pub path: Option<serde_json::Value>,

    /// Data of query result
    pub data: serde_json::Value,

    /// Extensions result
    pub extensions: Option<serde_json::Map<String, serde_json::Value>>,

    /// Cache control value
    pub cache_control: CacheControl,
}

impl QueryResponse {
    pub(crate) fn merge(&mut self, resp: QueryResponse) {
        todo!()
    }
}

/// Query builder
pub struct QueryBuilder {
    pub(crate) query_source: String,
    pub(crate) operation_name: Option<String>,
    pub(crate) variables: Variables,
    pub(crate) ctx_data: Option<Data>,
    pub(crate) files_holder: Option<TempDir>,
}

impl QueryBuilder {
    /// Create query builder with query source.
    pub fn new<T: Into<String>>(query_source: T) -> QueryBuilder {
        QueryBuilder {
            query_source: query_source.into(),
            operation_name: None,
            variables: Default::default(),
            ctx_data: None,
            files_holder: None,
        }
    }

    /// Specify the operation name.
    pub fn operator_name<T: Into<String>>(self, name: T) -> Self {
        QueryBuilder {
            operation_name: Some(name.into()),
            ..self
        }
    }

    /// Specify the variables.
    pub fn variables(self, variables: Variables) -> Self {
        QueryBuilder { variables, ..self }
    }

    /// Add a context data that can be accessed in the `Context`, you access it with `Context::data`.
    ///
    /// **This data is only valid for this query**
    pub fn data<D: Any + Send + Sync>(mut self, data: D) -> Self {
        if let Some(ctx_data) = &mut self.ctx_data {
            ctx_data.insert(data);
        } else {
            let mut ctx_data = Data::default();
            ctx_data.insert(data);
            self.ctx_data = Some(ctx_data);
        }
        self
    }

    /// Set file holder
    pub fn set_files_holder(&mut self, files_holder: TempDir) {
        self.files_holder = Some(files_holder);
    }

    /// Set uploaded file path
    pub fn set_upload(
        &mut self,
        var_path: &str,
        filename: &str,
        content_type: Option<&str>,
        path: &Path,
    ) {
        self.variables
            .set_upload(var_path, filename, content_type, path);
    }

    /// Execute the query, returns a stream, the first result being the query result,
    /// followed by the incremental result. Only when there are @defer and @stream directives
    /// in the query will there be subsequent incremental results.
    pub async fn execute_stream<Query, Mutation, Subscription>(
        self,
        schema: &Schema<Query, Mutation, Subscription>,
    ) -> impl Stream<Item = Result<QueryResponse>>
    where
        Query: ObjectType + Send + Sync + 'static,
        Mutation: ObjectType + Send + Sync + 'static,
        Subscription: SubscriptionType + Send + Sync + 'static,
    {
        let schema = schema.clone();
        let stream = async_stream::try_stream! {
            let (first_resp, defer_list) = self.execute_first(&schema).await?;
            yield first_resp;

            let mut current_defer_list = defer_list.0;

            loop {
                let mut new_defer_list = Vec::new();
                for defer in current_defer_list {
                    let mut res = defer.await?;
                    new_defer_list.extend((res.1).0);
                    yield res.0;
                }
                if new_defer_list.is_empty() {
                    break;
                }
                current_defer_list = new_defer_list;
            }
        };
        Box::pin(stream)
    }

    /// Execute the query, always return a complete result
    pub async fn execute<Query, Mutation, Subscription>(
        self,
        schema: &Schema<Query, Mutation, Subscription>,
    ) -> Result<QueryResponse>
    where
        Query: ObjectType + Send + Sync + 'static,
        Mutation: ObjectType + Send + Sync + 'static,
        Subscription: SubscriptionType + Send + Sync + 'static,
    {
        let mut stream = self.execute_stream(schema).await;
        let mut resp = stream.next().await.unwrap()?;
        while let Some(resp_part) = stream.next().await.transpose()? {
            resp.merge(resp_part);
        }
        Ok(resp)
    }

    async fn execute_first<Query, Mutation, Subscription>(
        self,
        schema: &Schema<Query, Mutation, Subscription>,
    ) -> Result<(QueryResponse, DeferFutureVec)>
    where
        Query: ObjectType + Send + Sync + 'static,
        Mutation: ObjectType + Send + Sync + 'static,
        Subscription: SubscriptionType + Send + Sync + 'static,
    {
        // create extension instances
        let extensions = schema
            .0
            .extensions
            .iter()
            .map(|factory| factory())
            .collect_vec();

        // parse query source
        extensions
            .iter()
            .for_each(|e| e.parse_start(&self.query_source));
        let document = parse_query(&self.query_source).map_err(Into::<Error>::into)?;
        extensions.iter().for_each(|e| e.parse_end());

        // check rules
        extensions.iter().for_each(|e| e.validation_start());
        let CheckResult {
            cache_control,
            complexity,
            depth,
        } = check_rules(&schema.0.registry, &document, schema.0.validation_mode)?;
        extensions.iter().for_each(|e| e.validation_end());

        // check limit
        if let Some(limit_complexity) = schema.0.complexity {
            if complexity > limit_complexity {
                return Err(QueryError::TooComplex.into_error(Pos::default()));
            }
        }

        if let Some(limit_depth) = schema.0.depth {
            if depth > limit_depth {
                return Err(QueryError::TooDeep.into_error(Pos::default()));
            }
        }

        // execute
        let mut fragments = HashMap::new();
        for definition in &document.definitions {
            if let Definition::Fragment(fragment) = &definition {
                fragments.insert(fragment.name.clone(), fragment.clone());
            }
        }

        let resolve_id = AtomicUsize::default();
        let (selection_set, variable_definitions, is_query) =
            current_operation(document, self.operation_name.as_deref()).ok_or_else(|| {
                Error::Query {
                    pos: Pos::default(),
                    path: None,
                    err: QueryError::MissingOperation,
                }
            })?;

        let env = Environment::new(EnvironmentInner {
            variables: self.variables,
            variable_definitions,
            fragments,
            ctx_data: Arc::new(self.ctx_data.unwrap_or_default()),
        });

        let defer_list = Mutex::new(DeferFutureVec::default());
        let ctx = ContextBase {
            path_node: None,
            resolve_id: &resolve_id,
            extensions: &extensions,
            item: &selection_set,
            registry: &schema.0.registry,
            data: &schema.0.data,
            env: &env,
            defer_list: Some(&defer_list),
        };

        extensions.iter().for_each(|e| e.execution_start());
        let data = if is_query {
            do_resolve(&ctx, &schema.0.query).await?
        } else {
            do_mutation_resolve(&ctx, &schema.0.mutation).await?
        };
        extensions.iter().for_each(|e| e.execution_end());

        let res = QueryResponse {
            path: None,
            data,
            extensions: if !extensions.is_empty() {
                Some(
                    extensions
                        .iter()
                        .filter_map(|e| e.result().map(|res| (e.name().to_string(), res)))
                        .collect::<serde_json::Map<_, _>>(),
                )
            } else {
                None
            },
            cache_control,
        };
        Ok((res, defer_list.into_inner()))
    }
}

fn current_operation<'a>(
    document: Document,
    operation_name: Option<&str>,
) -> Option<(SelectionSet, Vec<VariableDefinition>, bool)> {
    for definition in document.definitions {
        match definition {
            Definition::Operation(operation_definition) => match operation_definition {
                OperationDefinition::SelectionSet(s) => {
                    return Some((s, Vec::new(), true));
                }
                OperationDefinition::Query(query)
                    if query.name.is_none()
                        || operation_name.is_none()
                        || query.name.as_deref() == operation_name.as_deref() =>
                {
                    return Some((query.selection_set, query.variable_definitions, true));
                }
                OperationDefinition::Mutation(mutation)
                    if mutation.name.is_none()
                        || operation_name.is_none()
                        || mutation.name.as_deref() == operation_name.as_deref() =>
                {
                    return Some((mutation.selection_set, mutation.variable_definitions, false));
                }
                OperationDefinition::Subscription(subscription)
                    if subscription.name.is_none()
                        || operation_name.is_none()
                        || subscription.name.as_deref() == operation_name.as_deref() =>
                {
                    return None;
                }
                _ => {}
            },
            Definition::Fragment(_) => {}
        }
    }
    None
}
