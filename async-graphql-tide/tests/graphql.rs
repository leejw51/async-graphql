mod test_utils;
use serde_json::json;
use smol::{Task, Timer};
use std::time::Duration;
use tide::Request;

use async_graphql::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn quickstart() -> Result<()> {
    smol::run(async {
        let listen_addr = test_utils::find_listen_addr().await;

        let server = Task::<Result<()>>::spawn(async move {
            struct QueryRoot;
            #[Object]
            impl QueryRoot {
                #[field(desc = "Returns the sum of a and b")]
                async fn add(&self, a: i32, b: i32) -> i32 {
                    a + b
                }
            }

            let mut app = tide::new();
            app.at("/").post(|req: Request<()>| async move {
                let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();
                async_graphql_tide::graphql(req, schema, |query_builder| query_builder).await
            });
            app.listen(&listen_addr).await?;

            Ok(())
        });

        let client = Task::<Result<()>>::spawn(async move {
            Timer::after(Duration::from_millis(300)).await;

            let resp = reqwest::Client::new()
                .post(format!("http://{}", listen_addr).as_str())
                .body(r#"{"query":"{ add(a: 10, b: 20) }"}"#)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .send()
                .await?;

            assert_eq!(resp.status(), reqwest::StatusCode::OK);
            let string = resp.text().await?;
            println!("{}", string);

            assert_eq!(string, json!({"data": {"add": 30}}).to_string());

            Ok(())
        });

        client.await?;
        server.cancel().await;

        Ok(())
    })
}

#[test]
fn hello() -> Result<()> {
    smol::run(async {
        let listen_addr = test_utils::find_listen_addr().await;

        let server = Task::<Result<()>>::spawn(async move {
            struct Hello(String);
            struct QueryRoot;
            #[Object]
            impl QueryRoot {
                #[field(desc = "Returns hello")]
                async fn hello<'a>(&self, ctx: &'a Context<'_>) -> String {
                    let name = ctx.data_opt::<Hello>().map(|hello| hello.0.as_str());
                    format!("Hello, {}!", name.unwrap_or("world"))
                }
            }

            struct AppState {
                schema: Schema<QueryRoot, EmptyMutation, EmptySubscription>,
            }
            let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();

            let app_state = AppState { schema };
            let mut app = tide::with_state(app_state);

            app.at("/").post(|req: Request<AppState>| async move {
                let schema = req.state().schema.clone();
                let name = &req
                    .header(&"name".parse().unwrap())
                    .and_then(|values| values.first().map(|value| value.to_string()));

                async_graphql_tide::graphql(req, schema, |mut query_builder| {
                    if let Some(name) = name {
                        query_builder = query_builder.data(Hello(name.to_string()))
                    }
                    query_builder
                })
                .await
            });
            app.listen(&listen_addr).await?;

            Ok(())
        });

        let client = Task::<Result<()>>::spawn(async move {
            Timer::after(Duration::from_millis(300)).await;

            let resp = reqwest::Client::new()
                .post(format!("http://{}", listen_addr).as_str())
                .body(r#"{"query":"{ hello }"}"#)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header("Name", "Foo")
                .send()
                .await?;

            assert_eq!(resp.status(), reqwest::StatusCode::OK);
            let string = resp.text().await?;
            println!("{}", string);

            assert_eq!(string, json!({"data":{"hello":"Hello, Foo!"}}).to_string());

            let resp = reqwest::Client::new()
                .post(format!("http://{}", listen_addr).as_str())
                .body(r#"{"query":"{ hello }"}"#)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .send()
                .await?;

            assert_eq!(resp.status(), reqwest::StatusCode::OK);
            let string = resp.text().await?;
            println!("{}", string);

            assert_eq!(
                string,
                json!({"data":{"hello":"Hello, world!"}}).to_string()
            );

            Ok(())
        });

        client.await?;
        server.cancel().await;

        Ok(())
    })
}
