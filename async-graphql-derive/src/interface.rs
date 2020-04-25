use crate::args;
use crate::args::{InterfaceField, InterfaceFieldArgument};
use crate::output_type::OutputType;
use crate::utils::{build_value_repr, check_reserved_name, get_crate_name};
use inflector::Inflector;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, Result, Type};

pub fn generate(interface_args: &args::Interface, input: &DeriveInput) -> Result<TokenStream> {
    let crate_name = get_crate_name(interface_args.internal);
    let ident = &input.ident;
    let generics = &input.generics;
    let attrs = &input.attrs;
    let vis = &input.vis;
    let s = match &input.data {
        Data::Struct(s) => s,
        _ => return Err(Error::new_spanned(input, "It should be a struct.")),
    };
    let fields = match &s.fields {
        Fields::Unnamed(fields) => Some(&fields.unnamed),
        Fields::Unit => None,
        _ => return Err(Error::new_spanned(input, "All fields must be unnamed.")),
    };
    let extends = interface_args.extends;
    let mut enum_names = Vec::new();
    let mut enum_items = Vec::new();
    let mut type_into_impls = Vec::new();
    let gql_typename = interface_args
        .name
        .clone()
        .unwrap_or_else(|| ident.to_string());
    check_reserved_name(&gql_typename, interface_args.internal)?;
    let desc = interface_args
        .desc
        .as_ref()
        .map(|s| quote! {Some(#s)})
        .unwrap_or_else(|| quote! {None});
    let mut registry_types = Vec::new();
    let mut possible_types = Vec::new();
    let mut collect_inline_fields = Vec::new();
    let mut get_introspection_typename = Vec::new();

    if let Some(fields) = fields {
        for field in fields {
            if let Type::Path(p) = &field.ty {
                let enum_name = &p.path.segments.last().unwrap().ident;
                enum_items.push(quote! { #enum_name(#p) });
                type_into_impls.push(quote! {
                    impl #generics From<#p> for #ident #generics {
                        fn from(obj: #p) -> Self {
                            #ident::#enum_name(obj)
                        }
                    }
                });
                enum_names.push(enum_name);

                registry_types.push(quote! {
                    <#p as #crate_name::Type>::create_type_info(registry);
                    registry.add_implements(&<#p as #crate_name::Type>::type_name(), #gql_typename);
                });

                possible_types.push(quote! {
                    possible_types.insert(<#p as #crate_name::Type>::type_name().to_string());
                });

                collect_inline_fields.push(quote! {
                    if let #ident::#enum_name(obj) = self {
                        return obj.collect_inline_fields(name, pos, ctx, futures);
                    }
                });

                get_introspection_typename.push(quote! {
                    #ident::#enum_name(obj) => <#p as #crate_name::Type>::type_name()
                })
            } else {
                return Err(Error::new_spanned(field, "Invalid type"));
            }
        }
    }

    let mut methods = Vec::new();
    let mut schema_fields = Vec::new();
    let mut resolvers = Vec::new();

    for InterfaceField {
        name,
        desc,
        ty,
        args,
        deprecation,
        context,
        external,
        provides,
        requires,
    } in &interface_args.fields
    {
        let method_name = Ident::new(name, Span::call_site());
        let name = name.to_camel_case();
        let mut calls = Vec::new();
        let mut use_params = Vec::new();
        let mut decl_params = Vec::new();
        let mut get_params = Vec::new();
        let mut schema_args = Vec::new();
        let requires = match &requires {
            Some(requires) => quote! { Some(#requires) },
            None => quote! { None },
        };
        let provides = match &provides {
            Some(provides) => quote! { Some(#provides) },
            None => quote! { None },
        };

        if *context {
            decl_params.push(quote! { ctx: &'ctx #crate_name::Context<'ctx> });
            use_params.push(quote! { ctx });
        }

        for InterfaceFieldArgument {
            name,
            desc,
            ty,
            default,
        } in args
        {
            let ident = Ident::new(name, Span::call_site());
            let name = name.to_camel_case();
            decl_params.push(quote! { #ident: #ty });
            use_params.push(quote! { #ident });

            let param_default = match &default {
                Some(default) => {
                    let repr = build_value_repr(&crate_name, &default);
                    quote! {|| #repr }
                }
                None => quote! { || #crate_name::Value::Null },
            };
            get_params.push(quote! {
                let #ident: #ty = ctx.param_value(#name, ctx.position, #param_default)?;
            });

            let desc = desc
                .as_ref()
                .map(|s| quote! {Some(#s)})
                .unwrap_or_else(|| quote! {None});
            let schema_default = default
                .as_ref()
                .map(|v| {
                    let s = v.to_string();
                    quote! {Some(#s)}
                })
                .unwrap_or_else(|| quote! {None});
            schema_args.push(quote! {
                args.insert(#name, #crate_name::registry::InputValue {
                    name: #name,
                    description: #desc,
                    ty: <#ty as #crate_name::Type>::create_type_info(registry),
                    default_value: #schema_default,
                    validator: None,
                });
            });
        }

        for enum_name in &enum_names {
            calls.push(quote! {
                #ident::#enum_name(obj) => obj.#method_name(#(#use_params),*).await
            });
        }

        let ctx_lifetime = if *context {
            quote! { <'ctx> }
        } else {
            quote! {}
        };

        methods.push(quote! {
            async fn #method_name #ctx_lifetime(&self, #(#decl_params),*) -> #ty {
                match self {
                    #(#calls,)*
                }
            }
        });

        let desc = desc
            .as_ref()
            .map(|s| quote! {Some(#s)})
            .unwrap_or_else(|| quote! {None});
        let deprecation = deprecation
            .as_ref()
            .map(|s| quote! {Some(#s)})
            .unwrap_or_else(|| quote! {None});

        let ty = OutputType::parse(ty)?;
        let schema_ty = ty.value_type();

        schema_fields.push(quote! {
            fields.insert(#name.to_string(), #crate_name::registry::Field {
                name: #name.to_string(),
                description: #desc,
                args: {
                    let mut args = std::collections::HashMap::new();
                    #(#schema_args)*
                    args
                },
                ty: <#schema_ty as #crate_name::Type>::create_type_info(registry),
                deprecation: #deprecation,
                cache_control: Default::default(),
                external: #external,
                provides: #provides,
                requires: #requires,
            });
        });

        let resolve_obj = match &ty {
            OutputType::Value(_) => quote! {
                self.#method_name(#(#use_params),*).await
            },
            OutputType::Result(_, _) => {
                quote! {
                    self.#method_name(#(#use_params),*).await.
                        map_err(|err| err.into_error_with_path(ctx.position, ctx.path_node.as_ref().unwrap().to_json()))?
                }
            }
        };

        resolvers.push(quote! {
            if ctx.name.as_str() == #name {
                #(#get_params)*
                let ctx_obj = ctx.with_selection_set(&ctx.selection_set);
                return #crate_name::OutputValueType::resolve(&#resolve_obj, &ctx_obj, ctx.position).await;
            }
        });
    }

    let introspection_type_name = if get_introspection_typename.is_empty() {
        quote! { unreachable!() }
    } else {
        quote! {
            match self {
            #(#get_introspection_typename),*
            }
        }
    };

    let expanded = quote! {
        #(#attrs)*
        #vis enum #ident #generics { #(#enum_items),* }

        #(#type_into_impls)*

        impl #generics #ident #generics {
            #(#methods)*
        }

        impl #generics #crate_name::Type for #ident #generics {
            fn type_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(#gql_typename)
            }

            fn introspection_type_name(&self) -> std::borrow::Cow<'static, str> {
                #introspection_type_name
            }

            fn create_type_info(registry: &mut #crate_name::registry::Registry) -> String {
                registry.create_type::<Self, _>(|registry| {
                    #(#registry_types)*

                    #crate_name::registry::Type::Interface {
                        name: #gql_typename.to_string(),
                        description: #desc,
                        fields: {
                            let mut fields = std::collections::HashMap::new();
                            #(#schema_fields)*
                            fields
                        },
                        possible_types: {
                            let mut possible_types = std::collections::HashSet::new();
                            #(#possible_types)*
                            possible_types
                        },
                        extends: #extends,
                        keys: None,
                    }
                })
            }
        }

        #[#crate_name::async_trait::async_trait]
        impl #generics #crate_name::ObjectType for #ident #generics {
            async fn resolve_field(&self, ctx: &#crate_name::Context<'_>) -> #crate_name::Result<#crate_name::serde_json::Value> {
                #(#resolvers)*
                Err(#crate_name::QueryError::FieldNotFound {
                    field_name: ctx.name.clone(),
                    object: #gql_typename.to_string(),
                }.into_error(ctx.position))
            }

            fn collect_inline_fields<'a>(
                &'a self,
                name: &str,
                pos: #crate_name::Pos,
                ctx: &#crate_name::ContextSelectionSet<'a>,
                futures: &mut Vec<#crate_name::BoxFieldFuture<'a>>,
            ) -> #crate_name::Result<()> {
                #(#collect_inline_fields)*
                Ok(())
            }
        }

        #[#crate_name::async_trait::async_trait]
        impl #generics #crate_name::OutputValueType for #ident #generics {
            async fn resolve(value: &Self, ctx: &#crate_name::ContextSelectionSet<'_>, pos: #crate_name::Pos) -> #crate_name::Result<#crate_name::serde_json::Value> {
                #crate_name::do_resolve(ctx, value).await
            }
        }
    };
    Ok(expanded.into())
}
