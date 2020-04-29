use async_graphql::*;

#[async_std::test]
pub async fn test_object_field_flatten() {
    #[SimpleObject]
    struct MyObj {
        a: i32,
        b: i32,
        value_aa: i32,
    }

    struct Query;

    #[Object]
    impl Query {
        #[field(flatten)]
        async fn obj(&self) -> MyObj {
            MyObj {
                a: 1,
                b: 2,
                value_aa: 99,
            }
        }

        async fn c(&self) -> i32 {
            3
        }
    }

    Schema::new(Query, EmptyMutation, EmptySubscription);
    let query = "{ a b valueAa c }";
    let schema = Schema::new(Query, EmptyMutation, EmptySubscription);
    assert_eq!(
        schema.execute(&query).await.unwrap().data,
        serde_json::json!({
            "a": 1, "b": 2, "valueAa": 99, "c": 3
        })
    );
}
